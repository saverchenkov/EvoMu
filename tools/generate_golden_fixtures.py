#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Golden fixture generator and versioning harness for Destiny and Dogma.

Fixtures generated:
  - Destiny trajectories: Kinematic reference trajectories modeling CCP formulas (DERIVED_REFERENCE).
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
SDE_SQLITE = os.path.join(ROOT, "data", "evomu_sde.sqlite")
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
    if mode == 0:  # Goto
        d = extra["direction"]
        buf.extend(struct.pack("<ddd", d.x, d.y, d.z))
    elif mode == 1:  # Follow
        buf.extend(struct.pack("<qf", extra["target_id"], extra["range"]))
    elif mode == 2:  # Stop
        pass
    elif mode == 4:  # Orbit
        buf.extend(struct.pack("<Id", extra["target_id"], extra["range"]))
    elif mode == 3:  # Warp
        dest = extra["destination"]
        buf.extend(struct.pack("<ddd", dest.x, dest.y, dest.z))
        buf.extend(struct.pack("<i", 0))  # effect_stamp
        buf.extend(struct.pack("<q", struct.unpack("<q", struct.pack("<d", -1.0))[0]))
        buf.extend(struct.pack("<q", struct.unpack("<q", struct.pack("<d", 15000.0))[0]))
        buf.extend(struct.pack("<i", int(extra["warp_speed_au_s"] * 10.0)))
    return bytes(buf)


# ---------------------------------------------------------------------------
# Destiny Kinematics Integration (Matching _destiny.so & eve-destiny)
# ---------------------------------------------------------------------------
def integrate_subwarp(pos, vel, goal_vel, tau, dt=1.0):
    decay = math.exp(-dt / tau)
    v_diff = vel - goal_vel
    disp = goal_vel * dt + v_diff * (tau * (1.0 - decay))
    new_pos = pos + disp
    new_vel = goal_vel + v_diff * decay
    return new_pos, new_vel


def simulate_dynamic_warp(origin, destination, max_vel, warp_speed_au_s, total_ticks=8):
    to_dest = destination - origin
    dist = to_dest.length()
    warp_dir = to_dest.normalize()

    k_a = max(0.1, warp_speed_au_s)
    k_d = max(0.1, min(2.0, k_a / 3.0))
    v_cruise = k_a * AU_METERS
    v_drop = max(1.0, min(100.0, max_vel * 0.5))
    v_init = max(v_drop, max_vel * 0.75)

    eff_total = dist
    v_peak = min(v_cruise, max(v_init * 1.1, eff_total / ((1.0 / k_a) + (1.0 / k_d))))
    decel_start_dist = v_peak / k_d

    pos = origin
    vel = warp_dir * v_init
    elapsed_warp_time = 0.0
    phase = "Aligning"

    cps = []
    cps.append({"tick": 0, "position": pos.to_list(), "velocity": vel.to_list(), "mode_info": "Warp(Phase=Aligning)"})

    for t in range(1, total_ticks + 1):
        rem_dt = 1.0
        rem_dist = (destination - pos).length()

        if phase == "Aligning":
            phase = "Accelerating"

        if phase == "Accelerating":
            v_start = min(v_peak, v_init * math.exp(k_a * elapsed_warp_time))
            t_to_peak = max(0.0, math.log(v_peak / v_start) / k_a)
            dt_accel = min(rem_dt, t_to_peak)

            elapsed_warp_time += dt_accel
            v_end = min(v_peak, v_init * math.exp(k_a * elapsed_warp_time))
            accel_disp = min(rem_dist, (v_end - v_start) / k_a)
            pos = pos + warp_dir * accel_disp
            vel = warp_dir * v_end
            rem_dt -= dt_accel

            new_rem = (destination - pos).length()
            if new_rem <= decel_start_dist:
                phase = "Decelerating"
            elif abs(v_end - v_peak) < 1.0:
                phase = "Cruising" if v_peak >= v_cruise else "Decelerating"

        if phase == "Cruising" and rem_dt > 0.0:
            rem = (destination - pos).length()
            dist_to_decel = max(0.0, rem - decel_start_dist)
            time_in_cruise = min(rem_dt, dist_to_decel / v_peak)
            pos = pos + warp_dir * (v_peak * time_in_cruise)
            vel = warp_dir * v_peak
            rem_dt -= time_in_cruise
            if rem_dt > 0.0 or (destination - pos).length() <= decel_start_dist:
                phase = "Decelerating"

        if phase == "Decelerating" and rem_dt > 0.0:
            rem = (destination - pos).length()
            decay = math.exp(-k_d * rem_dt)
            new_rem = rem * decay
            cur_speed = max(v_drop, new_rem * k_d)
            if new_rem <= 2500.0 or cur_speed <= v_drop * 1.05:
                pos = destination - warp_dir * min(2500.0, new_rem)
                vel = warp_dir * v_drop
            else:
                pos = destination - warp_dir * new_rem
                vel = warp_dir * cur_speed

        cps.append({"tick": t, "position": pos.to_list(), "velocity": vel.to_list(), "mode_info": f"Warp({phase})"})

    return cps


def build_destiny_scenarios():
    mass = 1_067_000.0
    radius = 37.0
    max_vel = 365.0
    inertia = 3.45
    tau = (inertia * mass) / 1_000_000.0

    # 1. Stop
    stop_cps = []
    p = Vec3(0.0, 0.0, 0.0)
    v = Vec3(365.0, 0.0, 0.0)
    stop_cps.append({"tick": 0, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Stop"})
    for t in range(1, 6):
        p, v = integrate_subwarp(p, v, Vec3(0.0, 0.0, 0.0), tau, 1.0)
        stop_cps.append({"tick": t, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Stop"})
    stop_wire = encode_destiny_ball(2, Vec3(0.0, 0.0, 0.0), Vec3(365.0, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=0.0)

    # 2. Goto
    goto_cps = []
    p = Vec3(0.0, 0.0, 0.0)
    v = Vec3(0.0, 0.0, 0.0)
    dir_x = Vec3(1.0, 0.0, 0.0)
    goto_cps.append({"tick": 0, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "GotoDirection"})
    for t in range(1, 6):
        goal = dir_x * max_vel
        p, v = integrate_subwarp(p, v, goal, tau, 1.0)
        goto_cps.append({"tick": t, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "GotoDirection"})
    goto_wire = encode_destiny_ball(0, Vec3(0.0, 0.0, 0.0), Vec3(0.0, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=1.0, extra={"direction": dir_x})

    # 3. Follow
    target_pos = Vec3(6000.0, 0.0, 0.0)
    follow_range = 1500.0
    follow_cps = []
    p = Vec3(0.0, 0.0, 0.0)
    v = Vec3(0.0, 0.0, 0.0)
    follow_cps.append({"tick": 0, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Follow(range=1500)"})
    for t in range(1, 6):
        delta = target_pos - p
        dist = delta.length()
        desired_dist = radius + radius + follow_range
        err = dist - desired_dist
        decel_dist = max(max_vel * max(tau, 1.0), 50.0)
        factor = max(-1.0, min(1.0, err / decel_dist))
        goal = delta.normalize() * (max_vel * factor)
        p, v = integrate_subwarp(p, v, goal, tau, 1.0)
        follow_cps.append({"tick": t, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Follow(range=1500)"})
    follow_wire = encode_destiny_ball(1, Vec3(0.0, 0.0, 0.0), Vec3(0.0, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=1.0, extra={"target_id": 2000001, "range": follow_range})

    # 4. Orbit
    orbit_range = 2500.0
    orbit_anchor = Vec3(0.0, 0.0, 0.0)
    orbit_cps = []
    p = Vec3(2574.0, 0.0, 0.0)
    v = Vec3(0.0, 250.0, 0.0)
    orbit_cps.append({"tick": 0, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Orbit(range=2500)"})
    for t in range(1, 6):
        to_target = orbit_anchor - p
        dist = max(1.0, to_target.length())
        orbit_radius = radius + radius + orbit_range
        radial_dir = to_target / dist
        v_perp = v - radial_dir * v.dot(radial_dir)
        tangent = v_perp.normalize() if v_perp.length_sq() > 1e-4 else radial_dir.cross(Vec3(0.0, 1.0, 0.0)).normalize()
        centripetal_lead = (v.length() * (tau + 0.5)) / max(1.0, orbit_radius)
        radial_error = max(-3.0, min(3.0, (dist - orbit_radius) / 250.0))
        steer = (tangent + radial_dir * (radial_error + centripetal_lead)).normalize()
        goal = steer * max_vel
        p, v = integrate_subwarp(p, v, goal, tau, 1.0)
        orbit_cps.append({"tick": t, "position": p.to_list(), "velocity": v.to_list(), "mode_info": "Orbit(range=2500)"})
    orbit_wire = encode_destiny_ball(4, Vec3(2574.0, 0.0, 0.0), Vec3(0.0, 250.0, 0.0), max_vel, mass, radius, inertia, speed_frac=1.0, extra={"target_id": 2000001, "range": orbit_range})

    # 5. Warp
    warp_dest = Vec3(1.0e9, 0.0, 0.0)
    warp_cps = simulate_dynamic_warp(Vec3(0.0, 0.0, 0.0), warp_dest, max_vel, 3.0, 8)
    warp_wire = encode_destiny_ball(3, Vec3(0.0, 0.0, 0.0), Vec3(273.75, 0.0, 0.0), max_vel, mass, radius, inertia, speed_frac=1.0, extra={"destination": warp_dest, "warp_speed_au_s": 3.0})

    # 6. MicroJump
    mjd_pos0 = Vec3(5000.0, 10000.0, -2000.0)
    mjd_vel0 = Vec3(120.0, 160.0, 0.0)
    mjd_dir = mjd_vel0.normalize()
    mjd_post_pos = mjd_pos0 + mjd_dir * 100000.0
    mjd_cps = [
        {"tick": 0, "position": mjd_pos0.to_list(), "velocity": mjd_vel0.to_list(), "mode_info": "PreMicroJump"},
        {"tick": 1, "position": mjd_post_pos.to_list(), "velocity": mjd_vel0.to_list(), "mode_info": "PostMicroJump(+100km)"},
    ]
    mjd_wire = encode_destiny_ball(0, mjd_pos0, mjd_vel0, max_vel, mass, radius, inertia, speed_frac=1.0, extra={"direction": mjd_dir})

    return [
        {
            "name": "stop_exponential_deceleration",
            "mode_name": "Stop",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [365.0, 0.0, 0.0],
            "target_position": None,
            "checkpoints": stop_cps,
            "wire_packet_hex": stop_wire.hex(),
        },
        {
            "name": "goto_exponential_acceleration",
            "mode_name": "Goto",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [0.0, 0.0, 0.0],
            "target_position": None,
            "checkpoints": goto_cps,
            "wire_packet_hex": goto_wire.hex(),
        },
        {
            "name": "follow_target_convergence",
            "mode_name": "Follow",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [0.0, 0.0, 0.0],
            "target_position": target_pos.to_list(),
            "checkpoints": follow_cps,
            "wire_packet_hex": follow_wire.hex(),
        },
        {
            "name": "orbit_tangential_steering",
            "mode_name": "Orbit",
            "initial_position": [2574.0, 0.0, 0.0],
            "initial_velocity": [0.0, 250.0, 0.0],
            "target_position": orbit_anchor.to_list(),
            "checkpoints": orbit_cps,
            "wire_packet_hex": orbit_wire.hex(),
        },
        {
            "name": "warp_three_phase_s_curve",
            "mode_name": "Warp",
            "initial_position": [0.0, 0.0, 0.0],
            "initial_velocity": [273.75, 0.0, 0.0],
            "target_position": warp_dest.to_list(),
            "checkpoints": warp_cps,
            "wire_packet_hex": warp_wire.hex(),
        },
        {
            "name": "micro_jump_drive_forward_100km",
            "mode_name": "MicroJump",
            "initial_position": mjd_pos0.to_list(),
            "initial_velocity": mjd_vel0.to_list(),
            "target_position": None,
            "checkpoints": mjd_cps,
            "wire_packet_hex": mjd_wire.hex(),
        },
    ]


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
        sc1 = {s["name"]: s for s in d_v1.get("scenarios", [])}
        sc2 = {s["name"]: s for s in d_v2.get("scenarios", [])}
        for name, s1 in sc1.items():
            if name not in sc2:
                print(f"  [destiny] Scenario {name} missing in {v2}")
                has_diff = True
                continue
            s2 = sc2[name]
            if s1["wire_packet_hex"] != s2["wire_packet_hex"]:
                print(f"  [destiny] Wire packet mismatch in scenario {name}:")
                print(f"    {v1}: {s1['wire_packet_hex']}")
                print(f"    {v2}: {s2['wire_packet_hex']}")
                has_diff = True
            if len(s1["checkpoints"]) != len(s2["checkpoints"]):
                print(f"  [destiny] Checkpoints count mismatch in {name}: {len(s1['checkpoints'])} vs {len(s2['checkpoints'])}")
                has_diff = True
            else:
                for i, (cp1, cp2) in enumerate(zip(s1["checkpoints"], s2["checkpoints"])):
                    dp = math.sqrt(sum((a - b) ** 2 for a, b in zip(cp1["position"], cp2["position"])))
                    dv = math.sqrt(sum((a - b) ** 2 for a, b in zip(cp1["velocity"], cp2["velocity"])))
                    if dp > 1e-4 or dv > 1e-4:
                        print(f"  [destiny] Trajectory deviation in {name} tick {i}: delta_p={dp:.6e}m, delta_v={dv:.6e}m/s")
                        has_diff = True
    elif os.path.exists(destiny_path) and not (os.path.isfile(v1) or os.path.isfile(v2)):
        print(f"  [destiny] One or both versions not found in {destiny_path}")

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

    # 2. Generate Destiny Suite
    print("[golden_generator] Generating Destiny golden suite (DERIVED_REFERENCE from kinematics formulas)...")
    scenarios = build_destiny_scenarios()
    destiny_version_suite = {
        "fixture_type": "DERIVED_REFERENCE",
        "client_meta": meta,
        "scenarios": scenarios,
    }

    destiny_out1 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    destiny_out2 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "golden_trajectories", "ccp_golden_vectors.json")

    destiny_root_data = {
        "fixture_type": "DERIVED_REFERENCE",
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
    # Also provide top-level mirror for backward compatibility
    destiny_root_data["scenarios"] = scenarios

    for p in (destiny_out1, destiny_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(destiny_root_data, f, indent=2)
    print(f"  -> wrote {destiny_out1}")
    print(f"  -> wrote {destiny_out2}")
    print("[golden_generator] All golden suites dynamically generated and keyed by version!")


if __name__ == "__main__":
    main()
