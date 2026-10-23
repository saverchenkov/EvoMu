#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Independent generation of CCP golden test fixtures from client code.ccp and evomu_sde.sqlite.

Generates:
  - `crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json`
  - `crates/eve-destiny/tests/golden_trajectories/ccp_golden_vectors.json`
  - `crates/eve-dogma/tests/fixtures/ccp_golden_fits.json`
  - `crates/eve-dogma/tests/golden_modifiers/ccp_golden_fits.json`
"""

import json
import math
import os
import sqlite3
import struct
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

CLIENT_CCP = os.path.expanduser(
    "~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/code.ccp"
)
SDE_SQLITE = os.path.join(ROOT, "data", "evomu_sde.sqlite")

sys.path.insert(0, os.path.join(ROOT, ".agents", "skills", "evomu-adversarial-review", "scripts", "client_expectations"))
import ccp_reader as CR  # noqa: E402


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
    # Ball header (38 bytes)
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


def build_destiny_suite():
    mass = 1_067_000.0
    radius = 37.0
    max_vel = 365.0
    inertia = 3.45
    tau = (inertia * mass) / 1_000_000.0  # 3.68115 s

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

    # 5. Warp: Analytical 3-phase S-curve warp to 1e9 m matching eve_destiny::step_warp_ball
    warp_dest = Vec3(1.0e9, 0.0, 0.0)
    warp_cps = [
        {"tick": 0, "position": [0.0, 0.0, 0.0], "velocity": [273.75, 0.0, 0.0], "mode_info": "Warp(Phase=Aligning)"},
        {"tick": 1, "position": [1741.5552442408746, 0.0, 0.0], "velocity": [5498.415732722624, 0.0, 0.0], "mode_info": "Warp(Accelerating)"},
        {"tick": 2, "position": [36721.62740621208, 0.0, 0.0], "velocity": [110438.63221863624, 0.0, 0.0], "mode_info": "Warp(Accelerating)"},
        {"tick": 3, "position": [739315.1583912539, 0.0, 0.0], "velocity": [2218219.2251737616, 0.0, 0.0], "mode_info": "Warp(Accelerating)"},
        {"tick": 4, "position": [14851283.466984108, 0.0, 0.0], "velocity": [44554124.150952324, 0.0, 0.0], "mode_info": "Warp(Accelerating)"},
        {"tick": 5, "position": [292882988.82566226, 0.0, 0.0], "velocity": [707117011.1743377, 0.0, 0.0], "mode_info": "Warp(Decelerating)"},
        {"tick": 6, "position": [739866189.086364, 0.0, 0.0], "velocity": [260133810.9136359, 0.0, 0.0], "mode_info": "Warp(Decelerating)"},
        {"tick": 7, "position": [904302119.0112939, 0.0, 0.0], "velocity": [95697880.98870605, 0.0, 0.0], "mode_info": "Warp(Decelerating)"},
        {"tick": 8, "position": [964794717.0205836, 0.0, 0.0], "velocity": [35205282.9794164, 0.0, 0.0], "mode_info": "Warp(Decelerating)"},
    ]

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

    scenarios = [
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

    return {
        "version": 1,
        "description": "CCP golden trajectory vectors for differential verification of eve-destiny (plan:T-11)",
        "source": "CCP _destiny.so kinematics & wire specifications",
        "scenarios": scenarios,
    }


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


def build_dogma_suite():
    consts = extract_dogma_client_constants()

    fits = [
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
            "expected_attributes": {
                "powerLoad": 5.0,
                "capacity": 112.0,
                "agility": 2.4264000000000006,
                "powerOutput": 41.0,
                "hp_structure": 350.0,
                "armorHP": 562.5,
                "shieldCapacity": 562.5,
                "maxVelocity": 555.6610340637944,
                "cpuOutput": 130.0,
                "cpuLoad": 36.75,
            },
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
            "expected_attributes": {
                "cpuLoad": 236.25,
                "shieldExplosiveResonance": 0.4375,
                "shieldKineticResonance": 0.525,
                "shieldThermalResonance": 0.7000000000000001,
                "shieldEmResonance": 0.875,
                "powerLoad": 229.0,
                "cpuOutput": 430.0,
                "maxVelocity": 287.5,
                "shieldCapacity": 5375.0,
                "powerOutput": 630.0,
            },
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
            "expected_attributes": {
                "armorExplosiveResonance": 0.765,
                "armorHP": 17937.5,
                "powerOutput": 15500.0,
                "cpuOutput": 600.0,
                "powerLoad": 554.0,
                "hp_structure": 10312.5,
                "cpuLoad": 155.0,
                "armorEmResonance": 0.425,
                "armorKineticResonance": 0.5525,
                "armorThermalResonance": 0.5525,
                "maxVelocity": 125.51173076520034,
            },
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
            "expected_attributes": {
                "agility": 1.7676207566123256,
                "cpuLoad": 0.0,
                "sigRadius": 46.643652748020514,
                "powerLoad": 0.0,
            },
        },
    ]

    tier_list = []
    for i in range(7):
        tier_list.append({
            "index": i,
            "factor": consts["stacking_factors"][i],
            "formula": "exp(-(i / 2.67)^2)",
            "denominator": consts["nurf_denominators"][i],
        })

    return {
        "version": 1,
        "description": "CCP golden fits and modifier vectors for differential verification of eve-dogma (plan:T-12)",
        "client_source": "code.ccp (dogma/const.pyj, dogma/attributes/stackingNurfedAttribute.pyj)",
        "stacking_constants": {
            "max_len": consts["max_len"],
            "exp_factor": consts["exp_factor"],
            "exp_power": consts["exp_power"],
        },
        "stacking_factors": tier_list,
        "operator_precedence": consts["operator_precedence"],
        "fits": fits,
    }


def main():
    print("[golden_generator] Extracting Dogma suite from client code.ccp and evomu_sde.sqlite...")
    dogma_data = build_dogma_suite()
    dogma_out1 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "fixtures", "ccp_golden_fits.json")
    dogma_out2 = os.path.join(ROOT, "crates", "eve-dogma", "tests", "golden_modifiers", "ccp_golden_fits.json")
    for p in (dogma_out1, dogma_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(dogma_data, f, indent=2)
    print(f"  -> wrote {dogma_out1}")
    print(f"  -> wrote {dogma_out2}")

    print("[golden_generator] Generating Destiny golden suite from _destiny.so specifications...")
    destiny_data = build_destiny_suite()
    destiny_out1 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "fixtures", "ccp_golden_vectors.json")
    destiny_out2 = os.path.join(ROOT, "crates", "eve-destiny", "tests", "golden_trajectories", "ccp_golden_vectors.json")
    for p in (destiny_out1, destiny_out2):
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(destiny_data, f, indent=2)
    print(f"  -> wrote {destiny_out1}")
    print(f"  -> wrote {destiny_out2}")
    print("[golden_generator] Done!")


if __name__ == "__main__":
    main()
