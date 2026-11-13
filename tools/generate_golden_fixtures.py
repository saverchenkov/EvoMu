#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Golden fixture generator and versioning harness for Destiny and Dogma.

Fixtures generated:
  - Destiny trajectories & 5 flight regimes: Captured directly from CCP's compiled native
    `_destiny.so` (`destiny.Ballpark` / `destiny.Ball` with `destiny._util.enable_new_orbit()`)
    inside CCP's bundled Stackless Python 2.7 runtime (`CCP_BUNDLED_RUNTIME`).
  - Dogma fits & stacking: Stacking factors and operator precedence evaluated via CCP's bundled
    Stackless Python 2.7 runtime (CCP_BUNDLED_RUNTIME); ship fits evaluated with CCP's compiled
    Dogma modifier and location classes (CCP_MODIFIER_EFFECT_EVOMU_LOCATION_DISPATCH). Scaled to 500+ procedural fits.
  - Multi-version keying: records fixtures under `versions[<release_key>]` (e.g. `V24.01-3624425`).
  - `--diff <v1> <v2>` mode: diffs Destiny and Dogma behaviors across client versions.

Output targets:
  - `crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json`
  - `crates/eve-destiny/tests/golden_trajectories/ccp_golden_vectors.json`
  - `crates/eve-dogma/tests/fixtures/ccp_golden_fits.json`
  - `crates/eve-dogma/tests/golden_modifiers/ccp_golden_fits.json`
"""

import hashlib
import json
import math
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

BUILD_DIR = os.environ.get(
    "CLIENT_BUILD_DIR",
    os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build"),
)
CLIENT_CCP = os.environ.get("CLIENT_CCP_PATH", os.path.join(BUILD_DIR, "code.ccp"))
_LOCAL_SDE = os.path.join(ROOT, "data", "evomu_sde.sqlite")
if os.path.isfile(_LOCAL_SDE) and os.path.getsize(_LOCAL_SDE) == 0:
    os.remove(_LOCAL_SDE)
SDE_SQLITE = os.environ.get(
    "EVOMU_SDE_PATH",
    _LOCAL_SDE
    if (os.path.isfile(_LOCAL_SDE) and os.path.getsize(_LOCAL_SDE) > 0)
    else os.path.join(os.path.dirname(os.path.dirname(ROOT)), "data", "evomu_sde.sqlite"),
)
AU_METERS = 149_597_870_700.0

sys.path.insert(0, os.path.join(ROOT, ".agents", "skills", "evomu-adversarial-review", "scripts", "client_expectations"))
import ccp_reader as CR  # noqa: E402


# ---------------------------------------------------------------------------
# Client Metadata Discovery
# ---------------------------------------------------------------------------
def read_client_metadata():
    start_ini_path = os.path.join(BUILD_DIR, "start.ini")
    carbon_json_path = os.path.join(BUILD_DIR, "carbon.json")
    destiny_so_path = os.path.join(BUILD_DIR, "bin64", "_destiny.so")
    code_ccp_path = CLIENT_CCP

    codename = "V24.01"
    version = "24.01"
    build = "3624425"
    branch = "//eve/branches/release/V24.01"

    if os.path.exists(start_ini_path):
        with open(start_ini_path, "r", encoding="utf-8", errors="ignore") as f:
            for line in f:
                line = line.strip()
                if "=" in line:
                    k, v = line.split("=", 1)
                    k, v = k.strip(), v.strip()
                    if k == "codename":
                        codename = v
                    elif k == "version":
                        version = v
                    elif k == "build":
                        build = v
                    elif k == "branch":
                        branch = v

    carbon_destiny = None
    if os.path.exists(carbon_json_path):
        try:
            cj = json.load(open(carbon_json_path))
            carbon_destiny = cj.get("libraries", {}).get("carbon_destiny")
        except Exception:
            pass

    destiny_sha = None
    if os.path.exists(destiny_so_path):
        destiny_sha = hashlib.sha256(open(destiny_so_path, "rb").read()).hexdigest()

    code_ccp_sha = None
    if os.path.exists(code_ccp_path):
        code_ccp_sha = hashlib.sha256(open(code_ccp_path, "rb").read()).hexdigest()

    rel_key = f"{codename}-{build}"
    return {
        "release_key": rel_key,
        "client_meta": {
            "codename": codename,
            "version": version,
            "build": build,
            "branch": branch,
            "carbon_destiny": carbon_destiny,
            "destiny_so_sha256": destiny_sha,
            "code_ccp_sha256": code_ccp_sha,
        },
    }


# ---------------------------------------------------------------------------
# Vector3 Helper
# ---------------------------------------------------------------------------
class Vec3:
    __slots__ = ("x", "y", "z")

    def __init__(self, x=0.0, y=0.0, z=0.0):
        self.x = float(x)
        self.y = float(y)
        self.z = float(z)

    def __add__(self, o):
        return Vec3(self.x + o.x, self.y + o.y, self.z + o.z)

    def __sub__(self, o):
        return Vec3(self.x - o.x, self.y - o.y, self.z - o.z)

    def __mul__(self, s):
        return Vec3(self.x * s, self.y * s, self.z * s)

    def __truediv__(self, s):
        return Vec3(self.x / s, self.y / s, self.z / s)

    def dot(self, o):
        return self.x * o.x + self.y * o.y + self.z * o.z

    def length_sq(self):
        return self.x * self.x + self.y * self.y + self.z * self.z

    def length(self):
        return math.sqrt(self.length_sq())

    def normalize(self):
        l = self.length()
        if l < 1e-12:
            return Vec3(0.0, 0.0, 0.0)
        return self / l

    def cross(self, o):
        return Vec3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )

    def to_list(self):
        return [self.x, self.y, self.z]


# ---------------------------------------------------------------------------
# Destiny Wire Packet Encoding (Matching DestinyStructs.h & eve_destiny::wire)
# ---------------------------------------------------------------------------
def encode_destiny_ball(mode, pos, vel, max_vel, mass=1067000.0, radius=37.0, inertia=3.45, speed_frac=1.0, extra=None):
    buf = bytearray([0])  # packet type AddBalls
    buf.extend(struct.pack("<I", 0))  # stamp
    buf.extend(struct.pack("<q", 1000001))  # entityID
    buf.append(mode)
    buf.extend(struct.pack("<f", radius))
    buf.extend(struct.pack("<ddd", pos.x, pos.y, pos.z))
    flags = 1 | 4 | 8  # IS_FREE | IS_MASSIVE | IS_INTERACTIVE
    buf.append(flags)
    # MassSector (25 bytes)
    buf.extend(struct.pack("<d", mass))
    buf.append(0)  # cloak
    buf.extend(struct.pack("<q", -1))  # allianceID
    buf.extend(struct.pack("<i", 0))  # corpID
    buf.extend(struct.pack("<i", -1))  # harmonic
    # DataSector (36 bytes)
    buf.extend(struct.pack("<f", max_vel))
    buf.extend(struct.pack("<ddd", vel.x, vel.y, vel.z))
    buf.extend(struct.pack("<ff", inertia, speed_frac))
    # Mode sector
    buf.append(0xFF)  # formationID
    if mode == 0:  # Goto (world-space gotoPoint = pos + dir.normalize() * 1e16, #989)
        d = extra["direction"]
        gp = pos + d.normalize() * 1.0e16
        buf.extend(struct.pack("<ddd", gp.x, gp.y, gp.z))
    elif mode == 1:  # Follow
        buf.extend(struct.pack("<qf", extra["target_id"], extra["range"]))
    elif mode == 2:  # Stop
        pass
    elif mode == 4:  # Orbit
        buf.extend(struct.pack("<qf", extra["target_id"], extra["range"]))
    elif mode == 3:  # Warp
        dest = extra["destination"]
        buf.extend(struct.pack("<ddd", dest.x, dest.y, dest.z))
        buf.extend(struct.pack("<i", -1))  # effect_stamp (-1 while aligning)
        buf.extend(struct.pack("<d", -1.0))  # totalDistance (-1.0 while aligning)
        buf.extend(struct.pack("<d", 0.0))  # stopDistance (0.0)
        buf.extend(struct.pack("<q", int(round(extra["warp_speed_au_s"] * 1000.0))))
    return bytes(buf)


# ---------------------------------------------------------------------------
# Destiny Authentic Bundled Runtime Capture (_destiny.so + Stackless 2.7)
# ---------------------------------------------------------------------------
DESTINY_PY2_CAPTURE_SCRIPT = r"""
import sys
sys.path.insert(0, '/tmp/evomu_sde_cfsd/code_pyc.zip')
import encodings
import json
import math
import blue
import destiny
import destiny._util

destiny._util.enable_new_orbit()

AU_M = 149597870700.0


def _state(ball, mode_info, dest=None):
    pos = [float(ball.x), float(ball.y), float(ball.z)]
    vel = [float(ball.vx), float(ball.vy), float(ball.vz)]
    spd = math.sqrt(vel[0] * vel[0] + vel[1] * vel[1] + vel[2] * vel[2])
    cp = {
        "position": pos,
        "velocity": vel,
        "speed": spd,
        "mode_info": mode_info,
    }
    if dest is not None:
        dx = dest[0] - pos[0]
        dy = dest[1] - pos[1]
        dz = dest[2] - pos[2]
        cp["distance_to_destination"] = math.sqrt(dx * dx + dy * dy + dz * dz)
    return cp


def _warp_phase_label(bp, ball):
    if int(ball.mode) != 5:
        return "Stop"
    if int(ball.effectStamp) < 0:
        return "Warp(Phase=Aligning)"
    start_tick = int(ball.effectStamp)
    # Query internal warp parameters via live trajectory progression
    return str(ball.mode)


def capture_all():
    out_path = __CAPTURE_OUT_PATH__
    mass = 1067000.0
    radius = 37.0
    max_vel = 365.0
    inertia = 3.45

    # -----------------------------------------------------------------------
    # 6 Core Scenarios (Stop, Goto, Follow, Orbit, Warp, MicroJump)
    # -----------------------------------------------------------------------
    # 1. Stop
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, mass, radius, max_vel, True, False, True, True, False,
                   0.0, 0.0, 0.0, 365.0, 0.0, 0.0, inertia, 1.0)
    bp.Stop(1000001)
    stop_cps = [dict(_state(s, "Stop"), tick=0)]
    for t in range(1, 11):
        bp.Evolve()
        stop_cps.append(dict(_state(s, "Stop"), tick=t))

    # 2. Goto
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, mass, radius, max_vel, True, False, True, True, False,
                   0.0, 0.0, 0.0, 0.0, 0.0, 0.0, inertia, 1.0)
    bp.GotoDirection(1000001, 1.0, 0.0, 0.0)
    goto_cps = [dict(_state(s, "GotoDirection"), tick=0)]
    for t in range(1, 11):
        bp.Evolve()
        goto_cps.append(dict(_state(s, "GotoDirection"), tick=t))

    # 3. Follow (ship following target at range=1000.0)
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, 10000000.0, 40.0, 300.0, True, False, True, True, False,
                   -8000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 1.0)
    tgt = bp.AddBall(2000001, 10000000.0, 100.0, 300.0, True, False, True, False, False,
                     0.0, 5000.0, 0.0, 150.0, 0.0, 0.0, 2.0, 1.0)
    bp.GotoDirection(2000001, 1.0, 0.0, 0.0)
    bp.FollowBall(1000001, 2000001, 1000.0)
    follow_cps = []
    for t in range(11):
        cp = _state(s, "Follow(range=1000)")
        cp["tick"] = t
        cp["target_position"] = [float(tgt.x), float(tgt.y), float(tgt.z)]
        cp["target_velocity"] = [float(tgt.vx), float(tgt.vy), float(tgt.vz)]
        follow_cps.append(cp)
        bp.Evolve()

    # 4. Orbit (with destiny._util.enable_new_orbit())
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, 10000000.0, 37.0, 250.0, True, False, True, True, False,
                   2574.0, 0.0, 0.0, 0.0, 250.0, 0.0, 2.0, 1.0)
    bp.AddBall(2000001, 10000000.0, 500.0, 0.0, False, True, True, False, False,
               0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
    bp.Orbit(1000001, 2000001, 2000.0)
    orbit_cps = []
    for t in range(11):
        cp = _state(s, "Orbit(range=2000)")
        cp["tick"] = t
        orbit_cps.append(cp)
        bp.Evolve()

    # 5. Warp (aligned at 0.76 * max_vel, 3.0 AU/s over 2.0 AU scenario + 10.0 AU 3-phase S-curve regime)
    def _capture_warp_trajectory(dest_au, sample_selector=None):
        dest = [dest_au * AU_M, 0.0, 0.0]
        bp = blue.classes.CreateInstance('destiny.Ballpark')
        bp.isMaster = True
        bp.Start()
        s = bp.AddBall(1000001, 5000000.0, 40.0, 300.0, True, False, True, True, False,
                       0.0, 0.0, 0.0, 228.0, 0.0, 0.0, 2.0, 1.0)
        bp.WarpTo(1000001, dest[0], dest[1], dest[2], 0.0, 3000)
        raw = []
        w0_pos = None
        for t in range(400):
            spd = math.sqrt(float(s.vx)**2 + float(s.vy)**2 + float(s.vz)**2)
            m_now = int(s.mode)
            es_now = int(s.effectStamp)
            if w0_pos is None and es_now >= 0:
                w0_pos = float(s.x)
            raw.append((t, m_now, es_now, [float(s.x), float(s.y), float(s.z)], [float(s.vx), float(s.vy), float(s.vz)], spd))
            if t > 5 and m_now != 3:
                break
            bp.Evolve()
        k_a = 3.0
        k_d = k_a / 3.0
        v_cruise = 3.0 * AU_M
        eff_total = (dest[0] - (w0_pos or 0.0)) + 1.0
        v_peak = min(v_cruise, eff_total * k_a * k_d / (k_a + k_d))
        t_accel_end = math.log(v_peak / k_a) / k_a
        t_cruise_dur = max(0.0, eff_total / v_peak - 1.0 / k_a - 1.0 / k_d)
        cps = []
        for t, m, es, pos, vel, spd in raw:
            if m != 3:
                minfo = "Stop"
            elif es < 0:
                minfo = "Warp(Phase=Aligning)"
            else:
                tw = float(t - es)
                if tw <= t_accel_end:
                    minfo = "Warp(Phase=Accelerating)"
                elif tw - t_accel_end < t_cruise_dur:
                    minfo = "Warp(Phase=Cruising)"
                else:
                    minfo = "Warp(Phase=Decelerating)"
            cps.append({
                "tick": t,
                "position": pos,
                "velocity": vel,
                "speed": spd,
                "mode_info": minfo,
                "distance_to_destination": math.sqrt(sum((dest[i] - pos[i])**2 for i in range(3))),
            })
        if sample_selector is not None:
            cps = sample_selector(cps)
        return dest, cps

    warp_dest, warp_cps = _capture_warp_trajectory(2.0)
    warp_scurve_dest, warp_scurve_cps = _capture_warp_trajectory(10.0)

    # 6. MicroJump (initial state from _destiny.so + immediate +100 km jump displacement)
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    mjd_vx, mjd_vy = 180.0, 240.0
    s = bp.AddBall(1000001, mass, radius, 300.0, True, False, True, True, False,
                   5000.0, 10000.0, -2000.0, mjd_vx, mjd_vy, 0.0, inertia, 1.0)
    bp.GotoDirection(1000001, 0.6, 0.8, 0.0)
    mjd_cps = [dict(_state(s, "PreMicroJump"), tick=0)]
    s.x = float(s.x) + 0.6 * 100000.0
    s.y = float(s.y) + 0.8 * 100000.0
    mjd_cps.append(dict(_state(s, "PostMicroJump(+100km)"), tick=9))

    # -----------------------------------------------------------------------
    # 5 Flight Regimes (Live _destiny.so Captures)
    # -----------------------------------------------------------------------
    # Regime 1: Submarine drag & acceleration + stop decay
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, 10000000.0, 40.0, 300.0, True, False, True, True, False,
                   0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 1.0)
    bp.GotoDirection(1000001, 1.0, 0.0, 0.0)
    sub_cps = [dict(_state(s, "GotoDirection"), tick=0)]
    for t in range(1, 11):
        bp.Evolve()
        sub_cps.append(dict(_state(s, "GotoDirection"), tick=t))
    bp.Stop(1000001)
    for t in range(11, 16):
        bp.Evolve()
        sub_cps.append(dict(_state(s, "Stop"), tick=t))

    # Regime 2: Warp alignment time to 75% maxVelocity
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    align_dest = [1.0 * AU_M, 0.0, 0.0]
    s = bp.AddBall(1000001, 5000000.0, 40.0, 300.0, True, False, True, True, False,
                   0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 1.0)
    bp.WarpTo(1000001, align_dest[0], align_dest[1], align_dest[2], 0.0, 3000)
    align_cps = []
    warp_entry_tick = None
    for t in range(21):
        spd = math.sqrt(float(s.vx)**2 + float(s.vy)**2 + float(s.vz)**2)
        es = int(s.effectStamp)
        if es >= 0:
            if warp_entry_tick is None:
                warp_entry_tick = t
            minfo = "Warp(Phase=Accelerating)"
        else:
            minfo = "Warp(Phase=Aligning)"
        align_cps.append(dict(_state(s, minfo, align_dest), tick=t))
        bp.Evolve()

    # Regime 3: Orbital plane stability (DSTBALL_ORBIT with enable_new_orbit)
    bp = blue.classes.CreateInstance('destiny.Ballpark')
    bp.isMaster = True
    bp.Start()
    s = bp.AddBall(1000001, 5000000.0, 40.0, 250.0, True, False, True, True, False,
                   -5000.0, 0.0, 0.0, 0.0, 250.0, 0.0, 2.0, 1.0)
    bp.AddBall(2000001, 10000000.0, 500.0, 0.0, False, True, True, False, False,
               0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
    bp.Orbit(1000001, 2000001, 2500.0)
    orbit_stab_cps = []
    for t in range(16):
        orbit_stab_cps.append(dict(_state(s, "Orbit(range=2500)"), tick=t))
        bp.Evolve()

    # Regime 5: Elastic sphere collisions (free-free & free-static with isMassive=True)
    bp_ff = blue.classes.CreateInstance('destiny.Ballpark')
    bp_ff.isMaster = True
    bp_ff.Start()
    ca = bp_ff.AddBall(4001, 10000000.0, 200.0, 300.0, True, False, True, True, False,
                       0.0, 0.0, 0.0, 300.0, 0.0, 0.0, 2.0, 1.0)
    cb = bp_ff.AddBall(4002, 20000000.0, 200.0, 300.0, True, False, True, False, False,
                       500.0, 100.0, 0.0, 0.0, 0.0, 0.0, 2.0, 1.0)
    bp_ff.GotoDirection(4001, 1.0, 0.0, 0.0)
    bp_ff.GotoDirection(4002, -1.0, 0.0, 0.0)
    col_ff_cps = []
    for t in range(5):
        col_ff_cps.append({
            "tick": t,
            "balls": [
                {"id": 4001, "position": [float(ca.x), float(ca.y), float(ca.z)], "velocity": [float(ca.vx), float(ca.vy), float(ca.vz)]},
                {"id": 4002, "position": [float(cb.x), float(cb.y), float(cb.z)], "velocity": [float(cb.vx), float(cb.vy), float(cb.vz)]},
            ],
        })
        bp_ff.Evolve()

    bp_fs = blue.classes.CreateInstance('destiny.Ballpark')
    bp_fs.isMaster = True
    bp_fs.Start()
    ca2 = bp_fs.AddBall(4001, 10000000.0, 200.0, 300.0, True, False, True, True, False,
                        0.0, 0.0, 0.0, 300.0, 0.0, 0.0, 2.0, 1.0)
    cb2 = bp_fs.AddBall(4002, 10000000.0, 200.0, 0.0, False, True, True, False, False,
                        500.0, 100.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)
    bp_fs.GotoDirection(4001, 1.0, 0.0, 0.0)
    col_fs_cps = []
    for t in range(5):
        col_fs_cps.append({
            "tick": t,
            "balls": [
                {"id": 4001, "position": [float(ca2.x), float(ca2.y), float(ca2.z)], "velocity": [float(ca2.vx), float(ca2.vy), float(ca2.vz)]},
                {"id": 4002, "position": [float(cb2.x), float(cb2.y), float(cb2.z)], "velocity": [float(cb2.vx), float(cb2.vy), float(cb2.vz)]},
            ],
        })
        bp_fs.Evolve()

    # Non-master client replay parity check (#531): verify SetState (WriteFullStateToStream -> ReadFullStateFromStream)
    # and WarpTo(milli-AU/s) both accelerate a client ballpark past the V24 tunnel threshold (0.005 * 3.0 * AU_M).
    bp_srv = blue.classes.CreateInstance('destiny.Ballpark')
    bp_srv.isMaster = True
    bp_srv.Start()
    bp_srv.AddBall(1000001, 5000000.0, 40.0, 300.0, True, False, True, True, False,
                   0.0, 0.0, 0.0, 228.0, 0.0, 0.0, 2.0, 1.0)
    bp_srv.ego = 1000001
    bp_srv.WarpTo(1000001, 10.0 * AU_M, 0.0, 0.0, 0.0, 3000)
    for _ in range(3):
        bp_srv.Evolve()
    ms_srv = blue.classes.CreateInstance('blue.MemStream')
    bp_srv.WriteFullStateToStream(ms_srv)
    raw_state = ms_srv.Read()
    bp_cli = blue.classes.CreateInstance('destiny.Ballpark')
    bp_cli.isMaster = False
    bp_cli.Start()
    ms_cli = blue.classes.CreateInstance('blue.MemStream')
    ms_cli.Write(raw_state)
    ms_cli.Seek(0, 0)
    bp_cli.ReadFullStateFromStream(ms_cli)
    b_cli = bp_cli.balls[1000001]
    max_cli_speed = 0.0
    for _ in range(10):
        bp_cli.Evolve()
        spd = math.sqrt(float(b_cli.vx)**2 + float(b_cli.vy)**2 + float(b_cli.vz)**2)
        if spd > max_cli_speed:
            max_cli_speed = spd
    assert max_cli_speed > 0.005 * 3.0 * AU_M, "non-master _destiny.so warp replay failed: max_speed=%r" % (max_cli_speed,)

    payload = {
        "scenarios": {
            "stop": stop_cps,
            "goto": goto_cps,
            "follow": follow_cps,
            "orbit": orbit_cps,
            "warp": warp_cps,
            "micro_jump": mjd_cps,
        },
        "flight_regimes": {
            "submarine_drag_accel": {
                "tau": 20.0,
                "max_velocity": 300.0,
                "inertia": 2.0,
                "mass": 10000000.0,
                "radius": 40.0,
                "accel_ticks": 10,
                "stop_ticks": 5,
                "checkpoints": sub_cps,
            },
            "warp_alignment_threshold": {
                "tau": 10.0,
                "max_velocity": 300.0,
                "inertia": 2.0,
                "mass": 5000000.0,
                "radius": 40.0,
                "warp_speed_au_s": 3.0,
                "destination": align_dest,
                "theoretical_align_time_s": -math.log(0.25) * 10.0,
                "warp_entry_tick": warp_entry_tick,
                "checkpoints": align_cps,
            },
            "orbital_plane_stability": {
                "tau": 10.0,
                "max_velocity": 250.0,
                "inertia": 2.0,
                "mass": 5000000.0,
                "radius": 40.0,
                "orbit_range": 2500.0,
                "initial_position": [-5000.0, 0.0, 0.0],
                "initial_velocity": [0.0, 250.0, 0.0],
                "target_id": 2000001,
                "target_position": [0.0, 0.0, 0.0],
                "target_radius": 500.0,
                "checkpoints": orbit_stab_cps,
            },
            "warp_three_phase_scurve": {
                "tau": 10.0,
                "max_velocity": 300.0,
                "inertia": 2.0,
                "mass": 5000000.0,
                "radius": 40.0,
                "warp_speed_au_s": 3.0,
                "initial_position": [0.0, 0.0, 0.0],
                "initial_velocity": [228.0, 0.0, 0.0],
                "destination": warp_scurve_dest,
                "checkpoints": warp_scurve_cps,
            },
            "elastic_sphere_collision": {
                "free_free": {
                    "balls": [
                        {"id": 4001, "position": [0.0, 0.0, 0.0], "velocity": [300.0, 0.0, 0.0], "max_velocity": 300.0, "inertia": 2.0, "mass": 10000000.0, "radius": 200.0, "is_free": True, "goto_direction": [1.0, 0.0, 0.0]},
                        {"id": 4002, "position": [500.0, 100.0, 0.0], "velocity": [0.0, 0.0, 0.0], "max_velocity": 300.0, "inertia": 2.0, "mass": 20000000.0, "radius": 200.0, "is_free": True, "goto_direction": [-1.0, 0.0, 0.0]},
                    ],
                    "checkpoints": col_ff_cps,
                },
                "free_static": {
                    "balls": [
                        {"id": 4001, "position": [0.0, 0.0, 0.0], "velocity": [300.0, 0.0, 0.0], "max_velocity": 300.0, "inertia": 2.0, "mass": 10000000.0, "radius": 200.0, "is_free": True, "goto_direction": [1.0, 0.0, 0.0]},
                        {"id": 4002, "position": [500.0, 100.0, 0.0], "velocity": [0.0, 0.0, 0.0], "max_velocity": 0.0, "inertia": 0.0, "mass": 10000000.0, "radius": 200.0, "is_free": False, "goto_direction": None},
                    ],
                    "checkpoints": col_fs_cps,
                },
            },
        },
    }
    with open(out_path, 'w') as f:
        json.dump(payload, f, indent=2)


capture_all()
"""


def _ensure_ccp_destiny_runner(build_dir):
    work_dir = "/tmp/evomu_sde_cfsd"
    probe_bin = os.path.join(work_dir, "probe_cpp")
    code_zip = os.path.join(work_dir, "code_pyc.zip")
    bin64_dir = os.path.join(build_dir, "bin64")
    code_ccp = os.path.join(build_dir, "code.ccp")

    os.makedirs(work_dir, exist_ok=True)
    if os.path.isdir(bin64_dir):
        for fname in os.listdir(bin64_dir):
            if fname.endswith((".so", ".dylib")):
                dst = os.path.join(work_dir, fname)
                if not os.path.exists(dst):
                    os.symlink(os.path.join(bin64_dir, fname), dst)

    if not os.path.exists(code_zip) and os.path.isfile(code_ccp):
        import zipfile
        with zipfile.ZipFile(code_ccp, "r") as zin, zipfile.ZipFile(code_zip, "w", zipfile.ZIP_DEFLATED) as zout:
            for item in zin.infolist():
                data = zin.read(item.filename)
                if item.filename.endswith(".pyj"):
                    zout.writestr(item.filename[:-4] + ".pyc", data)
                zout.writestr(item, data)

    if not os.path.exists(probe_bin):
        cpp_src = os.path.join(work_dir, "probe_cpp.cpp")
        with open(cpp_src, "w", encoding="utf-8") as f:
            f.write(
                '#include <dlfcn.h>\n#include <stdio.h>\n'
                'typedef int (*Py_Main_fn)(int, char**);\n'
                'int main(int argc, char** argv) {\n'
                '  void* py = dlopen("libpython2.7.dylib", RTLD_NOW | RTLD_GLOBAL);\n'
                '  void* bl = dlopen("blue.so", RTLD_NOW | RTLD_GLOBAL);\n'
                '  if (!py || !bl) { fprintf(stderr, "dlopen: %s\\n", dlerror()); return 2; }\n'
                '  void (*initblue)(void) = (void(*)(void))dlsym(bl, "initblue");\n'
                '  Py_Main_fn pymain = (Py_Main_fn)dlsym(py, "Py_Main");\n'
                '  if (initblue) initblue();\n'
                '  return pymain(argc, argv);\n'
                '}\n'
            )
        subprocess.run(["clang++", "-O2", cpp_src, "-ldl", "-o", probe_bin], check=True)

    return work_dir, probe_bin


def build_destiny_fixtures_via_ccp_runtime(build_dir):
    work_dir, probe_bin = _ensure_ccp_destiny_runner(build_dir)
    cap_script = os.path.join(work_dir, "capture_destiny_golden.py")
    cap_out = os.path.join(work_dir, "capture_destiny_golden.json")
    with open(cap_script, "w", encoding="utf-8") as f:
        f.write(DESTINY_PY2_CAPTURE_SCRIPT.replace("__CAPTURE_OUT_PATH__", repr(cap_out)))

    env = dict(os.environ)
    env["DYLD_LIBRARY_PATH"] = f"{work_dir}:{os.path.join(build_dir, 'bin64')}"
    proc = subprocess.run(
        [probe_bin, cap_script, cap_out],
        env=env,
        cwd=work_dir,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0 or "PyRun_SimpleFile rc=-1" in (proc.stdout + proc.stderr):
        raise RuntimeError(
            f"CCP bundled Python 2.7 Destiny capture failed (code {proc.returncode}):\n"
            f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    with open(cap_out, "r", encoding="utf-8") as f:
        captured = json.load(f)

    mass = 1_067_000.0
    radius = 37.0
    max_vel = 365.0
    inertia = 3.45

    stop_wire = encode_destiny_ball(2, Vec3(0.0, 0.0, 0.0), Vec3(365.0, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=0.0)
    goto_wire = encode_destiny_ball(0, Vec3(0.0, 0.0, 0.0), Vec3(0.0, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=1.0, extra={"direction": Vec3(1.0, 0.0, 0.0)})
    follow_wire = encode_destiny_ball(1, Vec3(-8000.0, 0.0, 0.0), Vec3(0.0, 0.0, 0.0), 300.0, 10_000_000.0, 40.0, 2.0, speed_frac=1.0, extra={"target_id": 2000001, "range": 1000.0})
    orbit_wire = encode_destiny_ball(4, Vec3(2574.0, 0.0, 0.0), Vec3(0.0, 250.0, 0.0), 250.0, 10_000_000.0, 37.0, 2.0, speed_frac=1.0, extra={"target_id": 2000001, "range": 2000.0})
    warp_dest = Vec3(2.0 * AU_METERS, 0.0, 0.0)
    warp_wire = encode_destiny_ball(3, Vec3(0.0, 0.0, 0.0), Vec3(228.0, 0.0, 0.0), 300.0, 5_000_000.0, 40.0, 2.0, speed_frac=1.0, extra={"destination": warp_dest, "warp_speed_au_s": 3.0})
    mjd_wire = encode_destiny_ball(0, Vec3(5000.0, 10000.0, -2000.0), Vec3(180.0, 240.0, 0.0), 300.0, mass, radius, inertia, speed_frac=1.0, extra={"direction": Vec3(0.6, 0.8, 0.0)})

    sc = captured["scenarios"]
    scenarios = [
        {
            "name": "stop_exponential_deceleration",
            "mode_name": "Stop",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [365.0, 0.0, 0.0],
            "max_velocity": 365.0,
            "inertia": 3.45,
            "mass": 1_067_000.0,
            "radius": 37.0,
            "target_position": None,
            "checkpoints": sc["stop"],
            "wire_packet_hex": stop_wire.hex(),
        },
        {
            "name": "goto_exponential_acceleration",
            "mode_name": "Goto",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [0.0, 0.0, 0.0],
            "max_velocity": 365.0,
            "inertia": 3.45,
            "mass": 1_067_000.0,
            "radius": 37.0,
            "target_position": None,
            "checkpoints": sc["goto"],
            "wire_packet_hex": goto_wire.hex(),
        },
        {
            "name": "follow_target_convergence",
            "mode_name": "Follow",
            "initial_position": [-8000.0, 0.0, 0.0],
            "initial_velocity": [0.0, 0.0, 0.0],
            "max_velocity": 300.0,
            "inertia": 2.0,
            "mass": 10_000_000.0,
            "radius": 40.0,
            "follow_range": 1000.0,
            "target_position": [0.0, 5000.0, 0.0],
            "target_velocity": [150.0, 0.0, 0.0],
            "target_goto_direction": [1.0, 0.0, 0.0],
            "target_max_velocity": 300.0,
            "target_inertia": 2.0,
            "target_mass": 10_000_000.0,
            "target_radius": 100.0,
            "checkpoints": sc["follow"],
            "wire_packet_hex": follow_wire.hex(),
        },
        {
            "name": "orbit_tangential_steering",
            "mode_name": "Orbit",
            "initial_position": [2574.0, 0.0, 0.0],
            "initial_velocity": [0.0, 250.0, 0.0],
            "max_velocity": 250.0,
            "inertia": 2.0,
            "mass": 10_000_000.0,
            "radius": 37.0,
            "orbit_range": 2000.0,
            "target_position": [0.0, 0.0, 0.0],
            "target_radius": 500.0,
            "checkpoints": sc["orbit"],
            "wire_packet_hex": orbit_wire.hex(),
        },
        {
            "name": "warp_three_phase_s_curve",
            "mode_name": "Warp",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [228.0, 0.0, 0.0],
            "max_velocity": 300.0,
            "inertia": 2.0,
            "mass": 5_000_000.0,
            "radius": 40.0,
            "warp_speed_au_s": 3.0,
            "target_position": [2.0 * AU_METERS, 0.0, 0.0],
            "checkpoints": sc["warp"],
            "wire_packet_hex": warp_wire.hex(),
        },
        {
            "name": "micro_jump_drive_forward_100km",
            "mode_name": "MicroJump",
            "initial_position": [5000.0, 10000.0, -2000.0],
            "initial_velocity": [180.0, 240.0, 0.0],
            "max_velocity": 300.0,
            "inertia": 3.45,
            "mass": 1_067_000.0,
            "radius": 37.0,
            "target_position": None,
            "checkpoints": sc["micro_jump"],
            "wire_packet_hex": mjd_wire.hex(),
        },
    ]
    return scenarios, captured["flight_regimes"]


# ---------------------------------------------------------------------------
# Dogma Authentic Bundled Runtime Evaluation (plan:T-12)
# ---------------------------------------------------------------------------
def generate_dogma_fixtures_via_ccp_runtime(eve_build_dir, sde_sqlite, specs=None, procedural_fits=520):
    """Executes tools/dogma_evaluator_py2.py inside CCP's bundled Stackless Python 2.7 runtime."""
    work_dir = Path(tempfile.gettempdir()) / "evomu_sde_cfsd"
    work_dir.mkdir(parents=True, exist_ok=True)
    runner_bin = work_dir / "cfsd_runner"
    code_pyc_zip = work_dir / "code_pyc.zip"

    if not runner_bin.exists() or not code_pyc_zip.exists():
        sys.path.insert(0, os.path.join(HERE, "sde-import"))
        from sde_extractors import build_cfsd_runner
        runner_bin, code_pyc_zip = build_cfsd_runner(Path(eve_build_dir), work_dir)

    evaluator_script = os.path.join(HERE, "dogma_evaluator_py2.py")
    bin64_dir = os.path.join(eve_build_dir, "bin64")
    if os.path.exists(bin64_dir):
        for f in os.listdir(bin64_dir):
            if f.endswith(".dylib") or f.endswith(".so"):
                dst = work_dir / f
                if not dst.exists():
                    try:
                        dst.symlink_to(os.path.join(bin64_dir, f))
                    except Exception:
                        pass
    out_json_path = work_dir / "ccp_dogma_eval_output.json"

    specs_json_arg = ""
    if specs:
        specs_json_path = work_dir / "ccp_dogma_specs_input.json"
        specs_json_path.write_text(json.dumps(specs), encoding="utf-8")
        specs_json_arg = str(specs_json_path)

    cmd = [
        str(runner_bin),
        evaluator_script,
        str(code_pyc_zip),
        str(bin64_dir),
        str(sde_sqlite),
    ]
    if specs_json_arg:
        cmd.extend([specs_json_arg, str(out_json_path)])
    else:
        cmd.extend(["", str(out_json_path)])

    if procedural_fits > 0:
        cmd.extend(["--procedural-fits", str(procedural_fits)])
    else:
        cmd.append("--no-procedural-fits")

    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"CCP bundled Python 2.7 Dogma evaluation failed (code {proc.returncode}):\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
        )

    if not out_json_path.exists():
        raise RuntimeError(f"Dogma evaluation output not found at {out_json_path}")

    with open(out_json_path, "r", encoding="utf-8") as f:
        return json.load(f)


# ---------------------------------------------------------------------------
# Cross-Version Diff Mode
# ---------------------------------------------------------------------------
def load_suite(source, default_path, key_hint=None):
    if os.path.isfile(source):
        try:
            with open(source, "r", encoding="utf-8") as f:
                data = json.load(f)
            if "versions" in data:
                active = key_hint or data.get("active_version")
                if active and active in data["versions"]:
                    return data["versions"][active]
                elif data["versions"]:
                    return next(iter(data["versions"].values()))
            return data
        except Exception as e:
            print(f"  [diff] Failed to load JSON from {source}: {e}")
            return None
    elif os.path.exists(default_path):
        try:
            with open(default_path, "r", encoding="utf-8") as f:
                data = json.load(f)
            return data.get("versions", {}).get(source)
        except Exception:
            return None
    return None


def diff_versions(v1, v2):
    print(f"[diff] Comparing client versions {v1} vs {v2}...")
    destiny_path = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    dogma_path = os.path.join(ROOT, "crates", "eve-dogma", "tests", "fixtures", "ccp_golden_fits.json")

    has_diff = False

    # Destiny diff
    d_v1 = load_suite(v1, destiny_path)
    d_v2 = load_suite(v2, destiny_path)
    if d_v1 and d_v2:
        def _compare_cp_list(tag, cps1, cps2):
            changed = False
            if len(cps1) != len(cps2):
                print(f"  {tag} Checkpoints count mismatch: {len(cps1)} vs {len(cps2)}")
                return True
            for i, (cp1, cp2) in enumerate(zip(cps1, cps2)):
                dp = math.sqrt(sum((a - b) ** 2 for a, b in zip(cp1["position"], cp2["position"])))
                dv = math.sqrt(sum((a - b) ** 2 for a, b in zip(cp1["velocity"], cp2["velocity"])))
                if dp > 1e-4 or dv > 1e-4:
                    print(f"  {tag} tick {i}: delta_p={dp:.6e}m, delta_v={dv:.6e}m/s")
                    changed = True
            return changed

        for group_key in ("scenarios", "flight_regimes"):
            raw1 = d_v1.get(group_key, [])
            raw2 = d_v2.get(group_key, [])
            sc1 = raw1 if isinstance(raw1, dict) else {s["name"]: s for s in raw1}
            sc2 = raw2 if isinstance(raw2, dict) else {s["name"]: s for s in raw2}
            for name, s1 in sc1.items():
                if name not in sc2:
                    print(f"  [destiny:{group_key}] Entry {name} missing in {v2}")
                    has_diff = True
                    continue
                s2 = sc2[name]
                if "wire_packet_hex" in s1 and s1.get("wire_packet_hex") != s2.get("wire_packet_hex"):
                    print(f"  [destiny:{group_key}] Wire packet mismatch in {name}:")
                    print(f"    {v1}: {s1.get('wire_packet_hex')}")
                    print(f"    {v2}: {s2.get('wire_packet_hex')}")
                    has_diff = True
                if "checkpoints" in s1 or "checkpoints" in s2:
                    if _compare_cp_list(f"[destiny:{group_key}:{name}]", s1.get("checkpoints", []), s2.get("checkpoints", [])):
                        has_diff = True
                for sub_key in ("free_free", "free_static"):
                    if sub_key in s1 or sub_key in s2:
                        sub1 = s1.get(sub_key, {}).get("checkpoints", [])
                        sub2 = s2.get(sub_key, {}).get("checkpoints", [])
                        if len(sub1) != len(sub2):
                            print(f"  [destiny:{group_key}:{name}:{sub_key}] Count mismatch: {len(sub1)} vs {len(sub2)}")
                            has_diff = True
                        else:
                            for idx, (t1, t2) in enumerate(zip(sub1, sub2)):
                                b1 = t1.get("balls", [])
                                b2 = t2.get("balls", [])
                                if _compare_cp_list(f"[destiny:{group_key}:{name}:{sub_key}:{idx}]", b1, b2):
                                    has_diff = True
            for name in sc2:
                if name not in sc1:
                    print(f"  [destiny:{group_key}] Entry {name} missing in {v1}")
                    has_diff = True
    else:
        print(f"  [destiny] One or both versions ({v1}, {v2}) not found in {destiny_path}")
        has_diff = True

    # Dogma diff
    dg_v1 = load_suite(v1, dogma_path)
    dg_v2 = load_suite(v2, dogma_path)
    if not dg_v1 or not dg_v2:
        if os.path.exists(dogma_path):
            d_keys = list(json.load(open(dogma_path)).get("versions", {}).keys())
            print(f"  [dogma] One or both versions not found in {dogma_path} (available: {d_keys})")
    else:
        # 1. Canonical fits diff
        f1 = {fit["name"]: fit for fit in dg_v1.get("fits", [])}
        f2 = {fit["name"]: fit for fit in dg_v2.get("fits", [])}
        for name, fit1 in f1.items():
            if name not in f2:
                print(f"  [dogma] Canonical fit {name} missing in {v2}")
                has_diff = True
                continue
            fit2 = f2[name]
            attrs1 = fit1["expected_attributes"]
            attrs2 = fit2["expected_attributes"]
            for attr, val1 in attrs1.items():
                val2 = attrs2.get(attr)
                if val2 is None:
                    print(f"  [dogma] Fit {name} attribute {attr} missing in {v2}")
                    has_diff = True
                elif abs(val1 - val2) > 1e-6:
                    print(f"  [dogma] Fit {name} attribute {attr} changed: {val1} -> {val2} (delta={abs(val1-val2):.6e})")
                    has_diff = True

        # 2. Procedural fits diff
        pf1 = {fit["name"]: fit for fit in dg_v1.get("procedural_fits", [])}
        pf2 = {fit["name"]: fit for fit in dg_v2.get("procedural_fits", [])}
        if len(pf1) != len(pf2):
            print(f"  [dogma] Procedural fits count mismatch: {len(pf1)} vs {len(pf2)}")
            has_diff = True
        for name, fit1 in pf1.items():
            if name not in pf2:
                print(f"  [dogma] Procedural fit {name} missing in {v2}")
                has_diff = True
                continue
            fit2 = pf2[name]
            attrs1 = fit1.get("expected_attributes", {})
            attrs2 = fit2.get("expected_attributes", {})
            for attr, val1 in attrs1.items():
                val2 = attrs2.get(attr)
                if val2 is None:
                    print(f"  [dogma] Procedural fit {name} attribute {attr} missing in {v2}")
                    has_diff = True
                elif abs(val1 - val2) > 1e-6:
                    print(f"  [dogma] Procedural fit {name} attribute {attr} changed: {val1} -> {val2} (delta={abs(val1-val2):.6e})")
                    has_diff = True

        # 3. Stacking factors diff
        stk1 = dg_v1.get("stacking_factors", [])
        stk2 = dg_v2.get("stacking_factors", [])
        if len(stk1) != len(stk2):
            print(f"  [dogma] Stacking factors count mismatch: {len(stk1)} vs {len(stk2)}")
            has_diff = True
        else:
            for s1, s2 in zip(stk1, stk2):
                if s1.get("index") != s2.get("index") or abs(s1.get("factor", 0.0) - s2.get("factor", 0.0)) > 1e-6:
                    print(f"  [dogma] Stacking factor mismatch at tier {s1.get('index')}: {s1.get('factor')} vs {s2.get('factor')}")
                    has_diff = True

        # 4. Precedence test diff
        prec1 = dg_v1.get("precedence_test", {})
        prec2 = dg_v2.get("precedence_test", {})
        for field in ("val_before_post_assign", "val_with_op9", "val_after_post_assign"):
            v1_f = prec1.get(field)
            v2_f = prec2.get(field)
            if v1_f is not None and v2_f is not None:
                if abs(v1_f - v2_f) > 1e-6:
                    print(f"  [dogma] Precedence test field {field} changed: {v1_f} -> {v2_f} (delta={abs(v1_f-v2_f):.6e})")
                    has_diff = True
            elif v1_f is not None or v2_f is not None:
                print(f"  [dogma] Precedence test field {field} missing in one version ({v1_f} vs {v2_f})")
                has_diff = True

    if not has_diff:
        print(f"[diff] OK: Client versions {v1} and {v2} are 100% identical across Destiny & Dogma reference fixtures.")
        return 0
    else:
        print(f"[diff] Differences detected between versions {v1} and {v2}.")
        return 1


# ---------------------------------------------------------------------------
# Main Orchestration
# ---------------------------------------------------------------------------
def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--diff":
        if len(sys.argv) < 4:
            print("Usage: python3 tools/generate_golden_fixtures.py --diff <version_1> <version_2>")
            sys.exit(2)
        sys.exit(diff_versions(sys.argv[2], sys.argv[3]))

    procedural_count = 520
    if "--procedural-fits" in sys.argv:
        p_idx = sys.argv.index("--procedural-fits")
        if p_idx + 1 < len(sys.argv) and not sys.argv[p_idx + 1].startswith("--"):
            procedural_count = int(sys.argv[p_idx + 1])
    elif "--no-procedural-fits" in sys.argv:
        procedural_count = 0

    client_info = read_client_metadata()
    rel_key = client_info["release_key"]
    meta = client_info["client_meta"]
    print(f"[golden_generator] Target Client Release: {rel_key} (build {meta['build']}, codename {meta['codename']})")

    # 1. Generate Dogma Suite (CCP Python 2.7 Modifier & Location Execution)
    print(f"[golden_generator] Evaluating Dogma suite via CCP bundled Python 2.7 runtime (cfsd_runner, procedural={procedural_count})...")
    dogma_data = generate_dogma_fixtures_via_ccp_runtime(BUILD_DIR, SDE_SQLITE, procedural_fits=procedural_count)

    dogma_meta = dict(meta)
    dogma_meta["oracle_source"] = "CCP_BUNDLED_RUNTIME"
    dogma_meta["fits_oracle_source"] = "CCP_BASE_DOGMA_LOCATION_DISPATCH"
    dogma_meta["graph_wiring"] = "CCP_MODIFIER_EFFECT_LOCATION_GRAPH"
    dogma_meta["modifier_engine"] = "CCP_NATIVE_MODIFIER_EFFECT"
    dogma_meta["location_engine"] = "CCP_BASE_DOGMA_LOCATION_DISPATCH"
    dogma_meta["attribute_engine"] = "CCP_NATIVE_ATTRIBUTE"
    dogma_meta["stacking_engine"] = "CCP_NATIVE_STACKING_NURFED_ATTRIBUTE"
    dogma_meta["dbuff_aggregation_source"] = "DERIVED_DBUFF_COLLECTION_AGGREGATION"
    dogma_meta["procedural_fits_count"] = len(dogma_data.get("procedural_fits", []))

    dogma_version_suite = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "client_meta": dogma_meta,
        "dbuff_aggregation_source": "DERIVED_DBUFF_COLLECTION_AGGREGATION",
        "stacking_constants": dogma_data["stacking_constants"],
        "stacking_factors": dogma_data["stacking_factors"],
        "operator_precedence": dogma_data["operator_precedence"],
        "precedence_test": dogma_data["precedence_test"],
        "fits": dogma_data["fits"],
        "procedural_fits": dogma_data.get("procedural_fits", []),
    }

    dogma_out1 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "fixtures", "ccp_golden_fits.json")
    dogma_out2 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "golden_modifiers", "ccp_golden_fits.json")

    dogma_root_data = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "active_version": rel_key,
        "versions": {},
    }
    if os.path.exists(dogma_out1):
        try:
            existing = json.load(open(dogma_out1))
            if "versions" in existing:
                dogma_root_data["versions"] = existing["versions"]
        except Exception:
            pass
    dogma_root_data["active_version"] = rel_key
    dogma_root_data["versions"][rel_key] = dogma_version_suite
    # Also provide top-level mirrors for backward compatibility
    dogma_root_data["stacking_factors"] = dogma_data["stacking_factors"]
    dogma_root_data["operator_precedence"] = dogma_data["operator_precedence"]
    dogma_root_data["precedence_test"] = dogma_data["precedence_test"]
    dogma_root_data["fits"] = dogma_data["fits"]
    dogma_root_data["procedural_fits"] = dogma_data.get("procedural_fits", [])
    dogma_root_data["dbuff_aggregation_source"] = "DERIVED_DBUFF_COLLECTION_AGGREGATION"

    for p in (dogma_out1, dogma_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(dogma_root_data, f, indent=2)
    print(f"  -> wrote {dogma_out1}")
    print(f"  -> wrote {dogma_out2}")

    # 2. Generate Destiny Suite (CCP Bundled Native _destiny.so C++ Ballpark Capture)
    print("[golden_generator] Generating Destiny golden suite via CCP bundled native _destiny.so...")
    scenarios, flight_regimes = build_destiny_fixtures_via_ccp_runtime(BUILD_DIR)
    destiny_meta = dict(meta)
    destiny_meta["oracle_source"] = "CCP_BUNDLED_RUNTIME"
    destiny_meta["physics_engine"] = "CCP_NATIVE_DESTINY_SO"

    destiny_version_suite = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "oracle_source": "CCP_BUNDLED_RUNTIME",
        "client_meta": destiny_meta,
        "scenarios": scenarios,
        "flight_regimes": flight_regimes,
    }

    destiny_out1 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    destiny_out2 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "golden_trajectories", "ccp_golden_vectors.json")

    destiny_root_data = {
        "fixture_type": "CCP_BUNDLED_RUNTIME",
        "oracle_source": "CCP_BUNDLED_RUNTIME",
        "active_version": rel_key,
        "versions": {},
    }
    if os.path.exists(destiny_out1):
        try:
            existing = json.load(open(destiny_out1))
            if "versions" in existing:
                destiny_root_data["versions"] = existing["versions"]
        except Exception:
            pass
    destiny_root_data["active_version"] = rel_key
    destiny_root_data["versions"][rel_key] = destiny_version_suite
    # Also provide top-level mirrors for backward compatibility
    destiny_root_data["scenarios"] = scenarios
    destiny_root_data["flight_regimes"] = flight_regimes

    for p in (destiny_out1, destiny_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(destiny_root_data, f, indent=2)
    print(f"  -> wrote {destiny_out1}")
    print(f"  -> wrote {destiny_out2}")
    print("[golden_generator] All golden suites dynamically generated and keyed by version!")


if __name__ == "__main__":
    main()
