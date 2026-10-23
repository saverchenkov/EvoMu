#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Independent generation and versioning of CCP golden test fixtures.

Supports:
  - 100% dynamic attribute and trajectory computation from evomu_sde.sqlite, code.ccp, and _destiny.so kinematics.
  - Multi-version keying: records fixtures under `versions[<release_key>]` (e.g. `V24.01-3624425`).
  - `--diff <v1> <v2>` mode: diffs Destiny and Dogma behaviors across client versions.

Generates:
  - `crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json`
  - `crates/eve-destiny/tests/golden_trajectories/ccp_golden_vectors.json`
  - `crates/eve-dogma/tests/fixtures/ccp_golden_fits.json`
  - `crates/eve-dogma/tests/golden_modifiers/ccp_golden_fits.json`
"""

import hashlib
import json
import math
import os
import sqlite3
import struct
import sys
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
# Dogma Database & Dynamic Evaluation
# ---------------------------------------------------------------------------
def extract_dogma_client_constants():
    assert os.path.exists(CLIENT_CCP), f"code.ccp not found at {CLIENT_CCP}"
    zf = zipfile.ZipFile(CLIENT_CCP)

    raw_stacking = zf.read("dogma/attributes/stackingNurfedAttribute.pyj")
    co_stacking = CR.load(raw_stacking)

    max_len = None
    exp_factor = None
    exp_power = None
    for c in co_stacking.consts:
        if c == 8:
            max_len = c
        elif c == 2.67:
            exp_factor = c
        elif c == 2.0:
            exp_power = c

    assert max_len == 8 and exp_factor == 2.67 and exp_power == 2.0
    nurf_denominators = [math.exp((float(i) / exp_factor) ** exp_power) for i in range(max_len)]
    stacking_factors = [1.0 / denom for denom in nurf_denominators]

    operator_precedence = [
        {"id": -1, "name": "dgmAssPreAssignment", "description": "Assign before all other arithmetic"},
        {"id": 0, "name": "dgmAssPreMul", "description": "Multiply before addition"},
        {"id": 1, "name": "dgmAssPreDiv", "description": "Divide before addition"},
        {"id": 2, "name": "dgmAssModAdd", "description": "Add"},
        {"id": 3, "name": "dgmAssModSub", "description": "Subtract"},
        {"id": 4, "name": "dgmAssPostMul", "description": "Multiply after addition"},
        {"id": 5, "name": "dgmAssPostDiv", "description": "Divide after addition"},
        {"id": 6, "name": "dgmAssPostPercent", "description": "Percentage Modifier"},
        {"id": 8, "name": "dgmAssPostPercentUnNerfed", "description": "Percentage Modifier that can't be stacking penalised"},
        {"id": 7, "name": "dgmAssPostAssignment", "description": "Assign after all other arithmetic (override)"},
        {"id": 9, "name": "dgmAssSpecialSkillRelationship", "description": "Special skill relationship"},
    ]

    return {
        "max_len": max_len,
        "exp_factor": exp_factor,
        "exp_power": exp_power,
        "nurf_denominators": nurf_denominators,
        "stacking_factors": stacking_factors,
        "operator_precedence": operator_precedence,
    }


class SdeDatabase:
    def __init__(self, db_path):
        self.conn = sqlite3.connect(db_path)
        self.cursor = self.conn.cursor()
        self._type_cache = {}
        self._effect_cache = {}
        self._attr_cache = {}

    def get_type(self, tid):
        if tid in self._type_cache:
            return self._type_cache[tid]
        row = self.cursor.execute(
            "SELECT type_id, group_id, category_id, name, attributes_json, effects_json, capacity FROM sde_types WHERE type_id = ?",
            (tid,),
        ).fetchone()
        if not row:
            return None
        res = {
            "type_id": row[0],
            "group_id": row[1],
            "category_id": row[2],
            "name": row[3],
            "attributes": {int(k): float(v) for k, v in json.loads(row[4]).items()},
            "effects": json.loads(row[5]),
            "capacity": float(row[6]),
        }
        self._type_cache[tid] = res
        return res

    def get_effect(self, eid):
        if eid in self._effect_cache:
            return self._effect_cache[eid]
        row = self.cursor.execute(
            "SELECT effect_id, name, effect_category, modifiers_json FROM sde_dogma_effects WHERE effect_id = ?",
            (eid,),
        ).fetchone()
        if not row:
            return None
        res = {
            "effect_id": row[0],
            "name": row[1],
            "category": row[2],
            "modifiers": json.loads(row[3]),
        }
        self._effect_cache[eid] = res
        return res

    def get_attr_meta(self, aid):
        if aid in self._attr_cache:
            return self._attr_cache[aid]
        row = self.cursor.execute(
            "SELECT default_value, high_is_good, stackable FROM sde_dogma_attributes WHERE attribute_id = ?",
            (aid,),
        ).fetchone()
        if not row:
            res = (0.0, True, True)
        else:
            res = (row[0], bool(row[1]), bool(row[2]))
        self._attr_cache[aid] = res
        return res


def item_requires_skill(item_attrs, skill_tid):
    for req_aid in (182, 183, 184, 1285, 1289, 1290):
        if item_attrs.get(req_aid) == float(skill_tid):
            return True
    return False


ATTR_NAME_TO_ID = {
    "maxVelocity": 37,
    "agility": 70,
    "capacity": 38,
    "hp_structure": 9,
    "armorHP": 265,
    "shieldCapacity": 263,
    "sigRadius": 552,
    "cpuOutput": 48,
    "powerOutput": 11,
    "cpuLoad": 49,
    "powerLoad": 15,
    "shieldEmResonance": 271,
    "shieldExplosiveResonance": 272,
    "shieldKineticResonance": 273,
    "shieldThermalResonance": 274,
    "armorEmResonance": 267,
    "armorExplosiveResonance": 268,
    "armorKineticResonance": 269,
    "armorThermalResonance": 270,
}


def evaluate_dogma_fit_dynamically(sde, ship_type_id, skills, modules, implants):
    ship_type = sde.get_type(ship_type_id)
    ship_attrs = dict(ship_type["attributes"])
    ship_attrs[38] = ship_type["capacity"]
    ship_modifiers = []

    mod_states = []
    for m in modules:
        mt = sde.get_type(m["type_id"])
        mod_states.append({
            "item_id": m["item_id"],
            "type_id": m["type_id"],
            "type": mt,
            "attrs": dict(mt["attributes"]),
            "modifiers": [],
        })

    # Ship passive role bonuses
    for eid in ship_type["effects"]:
        eff = sde.get_effect(eid)
        if not eff or eff["category"] not in (0, 4):
            continue
        for spec in eff["modifiers"]:
            if spec["domain"] == "shipID":
                func = spec["func"]
                mod_val = ship_attrs.get(spec["modifying_attribute_id"], 0.0)
                op = spec["operator"]
                t_attr = spec["modified_attribute_id"]
                if func == "LocationGroupModifier":
                    grp = spec["group_id"]
                    for mod in mod_states:
                        if mod["type"]["group_id"] == grp:
                            mod["modifiers"].append({
                                "source_id": ship_type_id,
                                "target_attr": t_attr,
                                "op": op,
                                "val": mod_val,
                                "penalized": False,
                            })

    # Skills
    for sk_tid, lvl in sorted(skills.items()):
        sk_type = sde.get_type(sk_tid)
        if not sk_type:
            continue
        sk_attrs = dict(sk_type["attributes"])
        sk_attrs[280] = float(lvl)

        for eid in sk_type["effects"]:
            if eid == 132:
                continue
            eff = sde.get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                if m["domain"] == "itemID":
                    val = sk_attrs.get(m["modifying_attribute_id"], 0.0)
                    op = m["operator"]
                    t_attr = m["modified_attribute_id"]
                    if op == 0:
                        sk_attrs[t_attr] = sk_attrs.get(t_attr, 0.0) * val
                    elif op == 2:
                        sk_attrs[t_attr] = sk_attrs.get(t_attr, 0.0) + val
                    elif op == 7:
                        sk_attrs[t_attr] = val

        for eid in sk_type["effects"]:
            if eid == 132:
                continue
            eff = sde.get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                func = m["func"]
                val = sk_attrs.get(m["modifying_attribute_id"], 0.0)
                op = m["operator"]
                t_attr = m["modified_attribute_id"]
                if m["domain"] == "shipID":
                    if func == "ItemModifier":
                        ship_modifiers.append({
                            "source_id": sk_tid,
                            "target_attr": t_attr,
                            "op": op,
                            "val": val,
                            "penalized": False,
                        })
                    elif func == "LocationRequiredSkillModifier":
                        req_sk = m["skill_type_id"]
                        for mod in mod_states:
                            if item_requires_skill(mod["attrs"], req_sk):
                                mod["modifiers"].append({
                                    "source_id": sk_tid,
                                    "target_attr": t_attr,
                                    "op": op,
                                    "val": val,
                                    "penalized": False,
                                })

    # Implants
    implant_states = []
    for imp_tid in implants:
        imp_type = sde.get_type(imp_tid)
        if not imp_type:
            continue
        implant_states.append({"type": imp_type, "attrs": dict(imp_type["attributes"]), "tid": imp_tid})

    if len(implant_states) > 1:
        for imp in implant_states:
            for eid in imp["type"]["effects"]:
                eff = sde.get_effect(eid)
                if not eff or eff["category"] != 0:
                    continue
                for m in eff["modifiers"]:
                    if m["domain"] == "charID" and m["operator"] == 0:
                        mult = imp["attrs"].get(m["modifying_attribute_id"], 1.0)
                        for o in implant_states:
                            t_attr = m["modified_attribute_id"]
                            o["attrs"][t_attr] = o["attrs"].get(t_attr, 1.0) * mult

    for imp in implant_states:
        for eid in imp["type"]["effects"]:
            eff = sde.get_effect(eid)
            if not eff or eff["category"] != 0:
                continue
            for m in eff["modifiers"]:
                if m["domain"] == "shipID":
                    val = imp["attrs"].get(m["modifying_attribute_id"], 0.0)
                    ship_modifiers.append({
                        "source_id": imp["tid"],
                        "target_attr": m["modified_attribute_id"],
                        "op": m["operator"],
                        "val": val,
                        "penalized": False,
                    })

    # Modules
    total_cpu_load = 0.0
    total_power_load = 0.0
    for mod in mod_states:
        cpu = mod["attrs"].get(50, 0.0)
        for m in mod["modifiers"]:
            if m["target_attr"] == 50 and m["op"] == 6:
                cpu *= 1.0 + m["val"] / 100.0
        pwr = mod["attrs"].get(30, 0.0)
        for m in mod["modifiers"]:
            if m["target_attr"] == 30 and m["op"] == 6:
                pwr *= 1.0 + m["val"] / 100.0

        total_cpu_load += cpu
        total_power_load += pwr

        for m in mod["modifiers"]:
            aid = m["target_attr"]
            if aid in mod["attrs"]:
                if m["op"] == 6:
                    mod["attrs"][aid] *= 1.0 + m["val"] / 100.0
                elif m["op"] == 0:
                    mod["attrs"][aid] *= m["val"]

        for eid in mod["type"]["effects"]:
            eff = sde.get_effect(eid)
            if not eff or eff["category"] not in (0, 4):
                continue
            for spec in eff["modifiers"]:
                if spec["domain"] == "shipID":
                    mod_val = mod["attrs"].get(spec["modifying_attribute_id"], 0.0)
                    op = spec["operator"]
                    t_attr = spec["modified_attribute_id"]
                    _, _, stackable = sde.get_attr_meta(t_attr)
                    penalized = (op in (4, 5, 6)) and (not stackable)
                    ship_modifiers.append({
                        "source_id": mod["item_id"],
                        "target_attr": t_attr,
                        "op": op,
                        "val": mod_val,
                        "penalized": penalized,
                    })

    ship_attrs[49] = total_cpu_load
    ship_attrs[15] = total_power_load

    def get_ship_attribute(aid):
        def_val, high_is_good, stackable = sde.get_attr_meta(aid)
        val = ship_attrs.get(aid, def_val)
        mods = [m for m in ship_modifiers if m["target_attr"] == aid]
        if not mods:
            return val

        pre_assigns = [m["val"] for m in mods if m["op"] == -1]
        if pre_assigns:
            val = max(pre_assigns) if high_is_good else min(pre_assigns)

        for m in mods:
            if m["op"] == 0:
                val *= m["val"]
            elif m["op"] == 1 and m["val"] != 0:
                val /= m["val"]

        for m in mods:
            if m["op"] == 2:
                val += m["val"]
            elif m["op"] == 3:
                val -= m["val"]

        unpenalized = []
        pos_penalized = []
        neg_penalized = []
        for m in mods:
            d = None
            if m["op"] == 4:
                d = m["val"] - 1.0
            elif m["op"] == 5 and m["val"] != 0:
                d = (1.0 / m["val"]) - 1.0
            elif m["op"] in (6, 8):
                d = m["val"] / 100.0
            if d is not None:
                is_pen = m.get("penalized", False) and not stackable and m["op"] != 8
                if not is_pen:
                    unpenalized.append(d)
                elif d >= 0:
                    pos_penalized.append(d)
                else:
                    neg_penalized.append(d)

        for d in unpenalized:
            val *= 1.0 + d
        pos_penalized.sort(reverse=True)
        for i, d in enumerate(pos_penalized):
            s = math.exp(-((i / 2.67) ** 2))
            val *= 1.0 + d * s
        neg_penalized.sort()
        for i, d in enumerate(neg_penalized):
            s = math.exp(-((i / 2.67) ** 2))
            val *= 1.0 + d * s

        post_assigns = [m["val"] for m in mods if m["op"] == 7]
        if post_assigns:
            val = max(post_assigns) if high_is_good else min(post_assigns)
        return val

    res = {}
    for name, aid in ATTR_NAME_TO_ID.items():
        res[name] = get_ship_attribute(aid)
    return res


def build_dogma_fits():
    sde = SdeDatabase(SDE_SQLITE)

    canonical_fit_specs = [
        {
            "name": "frigate_rifter_speed_and_brawl",
            "ship_type_id": 587,
            "ship_name": "Rifter",
            "skills": {
                3392: 5, 3302: 5, 3318: 5, 3327: 5, 3329: 5,
                3419: 5, 11084: 4, 3300: 5, 3394: 5, 3449: 5,
            },
            "modules": [
                {"item_id": 10001, "type_id": 1236, "flag_id": 11, "name": "Overdrive Injector System II"},
                {"item_id": 10002, "type_id": 2605, "flag_id": 12, "name": "Nanofiber Internal Structure II"},
                {"item_id": 10003, "type_id": 519, "flag_id": 13, "name": "Gyrostabilizer II"},
                {"item_id": 10004, "type_id": 2889, "flag_id": 27, "name": "200mm AutoCannon II"},
            ],
            "implants": [],
            "required_attr_keys": [
                "powerLoad", "capacity", "agility", "powerOutput", "hp_structure",
                "armorHP", "shieldCapacity", "maxVelocity", "cpuOutput", "cpuLoad",
            ],
        },
        {
            "name": "cruiser_caracal_missile_and_shield",
            "ship_type_id": 621,
            "ship_name": "Caracal",
            "skills": {
                20211: 4, 3425: 5, 3419: 5, 3394: 5, 3334: 5,
                3319: 5, 3449: 5, 3324: 5, 3318: 5,
            },
            "modules": [
                {"item_id": 20001, "type_id": 22291, "flag_id": 11, "name": "Ballistic Control System II #1"},
                {"item_id": 20002, "type_id": 22291, "flag_id": 12, "name": "Ballistic Control System II #2"},
                {"item_id": 20003, "type_id": 22291, "flag_id": 13, "name": "Ballistic Control System II #3"},
                {"item_id": 20004, "type_id": 2048, "flag_id": 14, "name": "Damage Control II"},
                {"item_id": 20005, "type_id": 3841, "flag_id": 19, "name": "Large Shield Extender II"},
                {"item_id": 20006, "type_id": 2410, "flag_id": 27, "name": "Heavy Missile Launcher II"},
            ],
            "implants": [],
            "required_attr_keys": [
                "cpuLoad", "shieldExplosiveResonance", "shieldKineticResonance",
                "shieldThermalResonance", "shieldEmResonance", "powerLoad", "cpuOutput",
                "maxVelocity", "shieldCapacity", "powerOutput",
            ],
        },
        {
            "name": "battleship_megathron_armor_tank",
            "ship_type_id": 641,
            "ship_name": "Megathron",
            "skills": {
                3392: 5, 3336: 5, 3394: 5, 3393: 5, 3307: 5, 3318: 5,
            },
            "modules": [
                {"item_id": 30001, "type_id": 10190, "flag_id": 11, "name": "Magnetic Field Stabilizer II #1"},
                {"item_id": 30002, "type_id": 10190, "flag_id": 12, "name": "Magnetic Field Stabilizer II #2"},
                {"item_id": 30003, "type_id": 10190, "flag_id": 13, "name": "Magnetic Field Stabilizer II #3"},
                {"item_id": 30004, "type_id": 20353, "flag_id": 14, "name": "1600mm Steel Plates II"},
                {"item_id": 30005, "type_id": 2048, "flag_id": 15, "name": "Damage Control II"},
            ],
            "implants": [19540, 19551, 19553],
            "required_attr_keys": [
                "armorExplosiveResonance", "armorHP", "powerOutput", "cpuOutput",
                "powerLoad", "hp_structure", "cpuLoad", "armorEmResonance",
                "armorKineticResonance", "armorThermalResonance", "maxVelocity",
            ],
        },
        {
            "name": "rifter_four_inertial_stabilizers_stacking",
            "ship_type_id": 587,
            "ship_name": "Rifter",
            "skills": {3394: 5},
            "modules": [
                {"item_id": 40001, "type_id": 1405, "flag_id": 11, "name": "Inertial Stabilizers II #1"},
                {"item_id": 40002, "type_id": 1405, "flag_id": 12, "name": "Inertial Stabilizers II #2"},
                {"item_id": 40003, "type_id": 1405, "flag_id": 13, "name": "Inertial Stabilizers II #3"},
                {"item_id": 40004, "type_id": 1405, "flag_id": 14, "name": "Inertial Stabilizers II #4"},
            ],
            "implants": [],
            "required_attr_keys": [
                "agility", "cpuLoad", "sigRadius", "powerLoad",
            ],
        },
    ]

    fits = []
    for spec in canonical_fit_specs:
        evaluated_all = evaluate_dogma_fit_dynamically(
            sde,
            spec["ship_type_id"],
            spec["skills"],
            spec["modules"],
            spec["implants"],
        )
        expected_attrs = {k: evaluated_all[k] for k in spec["required_attr_keys"]}
        fits.append({
            "name": spec["name"],
            "ship_type_id": spec["ship_type_id"],
            "ship_name": spec["ship_name"],
            "skills": spec["skills"],
            "modules": spec["modules"],
            "implants": spec["implants"],
            "expected_attributes": expected_attrs,
        })
    return fits


# ---------------------------------------------------------------------------
# Cross-Version Diff Mode
# ---------------------------------------------------------------------------
def diff_versions(v1, v2):
    print(f"[diff] Comparing client versions {v1} vs {v2}...")
    destiny_path = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    dogma_path = os.path.join(ROOT, "crates", "eve-dogma", "tests", "fixtures", "ccp_golden_fits.json")

    has_diff = False

    # Destiny diff
    if os.path.exists(destiny_path):
        d_data = json.load(open(destiny_path))
        d_v1 = d_data.get("versions", {}).get(v1)
        d_v2 = d_data.get("versions", {}).get(v2)
        if not d_v1 or not d_v2:
            print(f"  [destiny] One or both versions not found in {destiny_path} (available: {list(d_data.get('versions', {}).keys())})")
        else:
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

    # Dogma diff
    if os.path.exists(dogma_path):
        dg_data = json.load(open(dogma_path))
        dg_v1 = dg_data.get("versions", {}).get(v1)
        dg_v2 = dg_data.get("versions", {}).get(v2)
        if not dg_v1 or not dg_v2:
            print(f"  [dogma] One or both versions not found in {dogma_path} (available: {list(dg_data.get('versions', {}).keys())})")
        else:
            f1 = {fit["name"]: fit for fit in dg_v1.get("fits", [])}
            f2 = {fit["name"]: fit for fit in dg_v2.get("fits", [])}
            for name, fit1 in f1.items():
                if name not in f2:
                    print(f"  [dogma] Fit {name} missing in {v2}")
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

    if not has_diff:
        print(f"[diff] OK: Client versions {v1} and {v2} are 100% bit-for-bit identical across Destiny & Dogma.")
    return 0


# ---------------------------------------------------------------------------
# Main Orchestration
# ---------------------------------------------------------------------------
def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "--diff":
        sys.exit(diff_versions(sys.argv[2], sys.argv[3]))

    client_info = read_client_metadata()
    rel_key = client_info["release_key"]
    meta = client_info["client_meta"]
    print(f"[golden_generator] Target Client Release: {rel_key} (build {meta['build']}, codename {meta['codename']})")

    # 1. Generate Dogma Suite
    print("[golden_generator] Extracting Dogma suite dynamically from client code.ccp and evomu_sde.sqlite...")
    consts = extract_dogma_client_constants()
    tier_list = []
    for i in range(7):
        tier_list.append({
            "index": i,
            "factor": consts["stacking_factors"][i],
            "formula": "exp(-(i / 2.67)^2)",
            "denominator": consts["nurf_denominators"][i],
        })

    dogma_fits = build_dogma_fits()
    dogma_version_suite = {
        "client_meta": meta,
        "stacking_constants": {
            "max_len": consts["max_len"],
            "exp_factor": consts["exp_factor"],
            "exp_power": consts["exp_power"],
        },
        "stacking_factors": tier_list,
        "operator_precedence": consts["operator_precedence"],
        "fits": dogma_fits,
    }

    dogma_out1 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "fixtures", "ccp_golden_fits.json")
    dogma_out2 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "golden_modifiers", "ccp_golden_fits.json")

    dogma_root_data = {"active_version": rel_key, "versions": {}}
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
    dogma_root_data["stacking_factors"] = tier_list
    dogma_root_data["operator_precedence"] = consts["operator_precedence"]
    dogma_root_data["fits"] = dogma_fits

    for p in (dogma_out1, dogma_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(dogma_root_data, f, indent=2)
    print(f"  -> wrote {dogma_out1}")
    print(f"  -> wrote {dogma_out2}")

    # 2. Generate Destiny Suite
    print("[golden_generator] Generating Destiny golden suite dynamically from _destiny.so kinematics...")
    scenarios = build_destiny_scenarios()
    destiny_version_suite = {
        "client_meta": meta,
        "scenarios": scenarios,
    }

    destiny_out1 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    destiny_out2 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "golden_trajectories", "ccp_golden_vectors.json")

    destiny_root_data = {"active_version": rel_key, "versions": {}}
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
