#!/usr/bin/python3
# -*- coding: utf-8 -*-
"""Golden-wire verification helpers for `client-bridge/evomu_bridge.py` (T-4).

Destiny `AddBalls`, Quasar envelope and Marshal fixture verification, split out of
`evomu_bridge.py` unchanged so the in-client bridge stays small. Used by
`evomu_bridge.py --verify-golden <fixture.json>`.
"""

import json
import struct
import sys

import os as _os

sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from evomu_bridge import (  # noqa: E402
    marshal_decode_pyrep,
    marshal_encode_pyrep,
)

def decode_add_balls(raw_bytes):
    """Decodes a binary Destiny `AddBalls` buffer across all 13 `DST_BALL_*` modes (`0..=12`)."""
    ba = bytearray(raw_bytes)
    if len(ba) < 5 or ba[0] != 0:
        raise ValueError("Invalid AddBalls header")
    stamp = struct.unpack("<I", bytes(ba[1:5]))[0]
    pos = 5
    balls = []
    while pos < len(ba):
        ball_id = struct.unpack("<q", bytes(ba[pos:pos + 8]))[0]
        mode_byte = ba[pos + 8]
        radius = struct.unpack("<f", bytes(ba[pos + 9:pos + 13]))[0]
        px, py, pz = struct.unpack("<ddd", bytes(ba[pos + 13:pos + 37]))
        flags = ba[pos + 37]
        pos += 38
        mass = 0.0
        if mode_byte != 11:
            mass = struct.unpack("<d", bytes(ba[pos:pos + 8]))[0]
            pos += 25  # mass(8) + cloak(1) + alliance(8) + corp(4) + harmonic(4)
        max_velocity = 0.0
        vx = vy = vz = 0.0
        inertia = 1.0
        speed_fraction = 0.0
        if mode_byte != 11 and (flags & 0x01) != 0:
            max_velocity = struct.unpack("<f", bytes(ba[pos:pos + 4]))[0]
            vx, vy, vz = struct.unpack("<ddd", bytes(ba[pos + 4:pos + 28]))
            inertia, speed_fraction = struct.unpack("<ff", bytes(ba[pos + 28:pos + 36]))
            pos += 36
        _formation_id = ba[pos]
        pos += 1
        extra = {}
        if mode_byte == 0:
            dx, dy, dz = struct.unpack("<ddd", bytes(ba[pos:pos + 24]))
            pos += 24
            extra = {"direction": [dx, dy, dz]}
        elif mode_byte in (1, 4):
            tid = struct.unpack("<q", bytes(ba[pos:pos + 8]))[0]
            rng = struct.unpack("<f", bytes(ba[pos + 8:pos + 12]))[0]
            pos += 12
            extra = {"target_id": tid, "range": float(rng)}
        elif mode_byte == 3:
            dx, dy, dz = struct.unpack("<ddd", bytes(ba[pos:pos + 24]))
            ws = struct.unpack("<I", bytes(ba[pos + 44:pos + 48]))[0]
            pos += 48
            extra = {"destination": [dx, dy, dz], "warp_speed_au_s": float(ws) / 10.0}
        elif mode_byte == 5:
            tid = struct.unpack("<q", bytes(ba[pos:pos + 8]))[0]
            pos += 48
            extra = {"target_id": tid}
        elif mode_byte == 6:
            pos += 24
        elif mode_byte == 8:
            pos += 4
        elif mode_byte == 12:
            pos += 16
        balls.append({
            "id": ball_id,
            "mode_byte": mode_byte,
            "flags": flags,
            "radius": float(radius),
            "position": [px, py, pz],
            "mass": mass,
            "max_velocity": float(max_velocity),
            "velocity": [vx, vy, vz],
            "inertia": float(inertia),
            "speed_fraction": float(speed_fraction),
            "extra": extra,
        })
    return stamp, balls


def encode_add_balls(stamp, balls):
    """Encodes a binary Destiny `AddBalls` buffer matching `eve_destiny::wire::encode_add_balls`."""
    out = bytearray([0])
    out.extend(bytearray(struct.pack("<I", int(stamp))))
    for b in balls:
        mode_byte = int(b["mode_byte"])
        flags = int(b["flags"])
        out.extend(bytearray(struct.pack("<q", int(b["id"]))))
        out.append(mode_byte)
        out.extend(bytearray(struct.pack("<f", float(b["radius"]))))
        px, py, pz = b["position"]
        out.extend(bytearray(struct.pack("<ddd", float(px), float(py), float(pz))))
        out.append(flags)
        if (flags & 0x04) != 0:
            out.extend(bytearray(struct.pack("<d", float(b.get("mass", 0.0)))))
            out.append(0)
            out.extend(bytearray(struct.pack("<qIf", -1, 0, -1.0)))
        if mode_byte != 11 and (flags & 0x01) != 0:
            out.extend(bytearray(struct.pack("<f", float(b.get("max_velocity", 0.0)))))
            vx, vy, vz = b.get("velocity", [0.0, 0.0, 0.0])
            out.extend(bytearray(struct.pack("<ddd", float(vx), float(vy), float(vz))))
            out.extend(bytearray(struct.pack(
                "<ff", float(b.get("inertia", 1.0)), float(b.get("speed_fraction", 0.0))
            )))
        out.append(1)
        extra = b.get("extra", {})
        if mode_byte == 0:
            dx, dy, dz = extra["direction"]
            out.extend(bytearray(struct.pack("<ddd", float(dx), float(dy), float(dz))))
        elif mode_byte in (1, 4):
            out.extend(bytearray(struct.pack(
                "<qf", int(extra["target_id"]), float(extra["range"])
            )))
        elif mode_byte == 3:
            dx, dy, dz = extra["destination"]
            ws = int(round(float(extra["warp_speed_au_s"]) * 10.0))
            out.extend(bytearray(struct.pack("<dddIdqI", float(dx), float(dy), float(dz), 0, 15000.0, 0, ws)))
        elif mode_byte == 5:
            out.extend(bytearray(struct.pack("<q", int(extra["target_id"]))))
    return bytes(out)


def _write_varint(val, out):
    v = int(val)
    while True:
        b = v & 0x7F
        v >>= 7
        if v != 0:
            b |= 0x80
        out.append(b)
        if v == 0:
            break


def _read_varint(ba, pos):
    res = 0
    shift = 0
    while True:
        b = ba[pos]
        pos += 1
        res |= (b & 0x7F) << shift
        if (b & 0x80) == 0:
            return res, pos
        shift += 7


def quasar_encode_envelope(env):
    out = bytearray()
    type_bytes = bytearray(env["type_url"].encode("utf-8"))
    _write_varint((1 << 3) | 2, out)
    _write_varint(len(type_bytes), out)
    out.extend(type_bytes)
    _write_varint((2 << 3) | 0, out)
    _write_varint(int(env["correlation_id"]), out)
    _write_varint((3 << 3) | 0, out)
    _write_varint(int(env["status_code"]), out)
    payload = bytearray(env["payload"])
    _write_varint((4 << 3) | 2, out)
    _write_varint(len(payload), out)
    out.extend(payload)
    return bytes(out)


def quasar_decode_envelope(raw_bytes):
    ba = bytearray(raw_bytes)
    pos = 0
    type_url = ""
    correlation_id = 0
    status_code = 0
    payload = []
    while pos < len(ba):
        tag, pos = _read_varint(ba, pos)
        fnum = tag >> 3
        wtype = tag & 0x07
        if wtype == 0:
            v, pos = _read_varint(ba, pos)
            if fnum == 2:
                correlation_id = v
            elif fnum == 3:
                status_code = v
        elif wtype == 2:
            n, pos = _read_varint(ba, pos)
            chunk = ba[pos:pos + n]
            pos += n
            if fnum == 1:
                type_url = bytes(chunk).decode("utf-8")
            elif fnum == 4:
                payload = [int(x) for x in chunk]
        elif wtype == 1:
            pos += 8
        elif wtype == 5:
            pos += 4
    return {
        "type_url": type_url,
        "correlation_id": correlation_id,
        "status_code": status_code,
        "payload": payload,
    }


def verify_golden_wire_fixtures(fixture_path):
    """Verifies every Marshal, Destiny, and Quasar golden wire fixture in `fixture_path`."""
    data = json.load(open(fixture_path, "r"))
    marshal_count = 0
    for entry in data.get("marshal_fixtures", []):
        raw = bytes(bytearray(entry["wire_bytes"]))
        decoded = marshal_decode_pyrep(raw)
        if decoded != entry["expected_pyrep"]:
            raise AssertionError("Marshal decode mismatch on %s: %r != %r" % (
                entry["name"], decoded, entry["expected_pyrep"]
            ))
        if entry.get("canonical_roundtrip", True):
            reencoded = [int(x) for x in bytearray(marshal_encode_pyrep(decoded))]
            if reencoded != entry["wire_bytes"]:
                raise AssertionError("Marshal re-encode mismatch on %s" % entry["name"])
        marshal_count += 1

    destiny_balls_count = 0
    for entry in data.get("destiny_add_balls_fixtures", []):
        raw = bytes(bytearray(entry["wire_bytes"]))
        stamp, balls = decode_add_balls(raw)
        if stamp != entry["stamp"] or len(balls) != len(entry["balls"]):
            raise AssertionError("AddBalls decode mismatch on %s" % entry["name"])
        for b, expected in zip(balls, entry["balls"]):
            if b["id"] != expected["id"] or b["mode_byte"] != expected["mode_byte"]:
                raise AssertionError("Ball mismatch on %s: %r != %r" % (entry["name"], b, expected))
            if abs(b["radius"] - expected["radius"]) > 0.001:
                raise AssertionError("Ball radius mismatch on %s: %r != %r" % (entry["name"], b, expected))
        destiny_balls_count += len(balls)

    destiny_updates_count = 0
    for entry in data.get("destiny_update_fixtures", []):
        raw = bytes(bytearray(entry["wire_bytes"]))
        decoded = marshal_decode_pyrep(raw)
        if decoded != entry["expected_pyrep"]:
            raise AssertionError("DoDestinyUpdate mismatch on %s" % entry["name"])
        reencoded = [int(x) for x in bytearray(marshal_encode_pyrep(decoded))]
        if reencoded != entry["wire_bytes"]:
            raise AssertionError("DoDestinyUpdate re-encode mismatch on %s" % entry["name"])
        destiny_updates_count += 1

    quasar_count = 0
    for entry in data.get("quasar_fixtures", []):
        raw = bytes(bytearray(entry["wire_bytes"]))
        decoded = quasar_decode_envelope(raw)
        if decoded != entry["envelope"]:
            raise AssertionError("Quasar decode mismatch on %s" % entry["name"])
        reencoded = [int(x) for x in bytearray(quasar_encode_envelope(decoded))]
        if reencoded != entry["wire_bytes"]:
            raise AssertionError("Quasar re-encode mismatch on %s" % entry["name"])
        quasar_count += 1

    return {
        "ok": True,
        "marshal_fixtures": marshal_count,
        "destiny_balls": destiny_balls_count,
        "destiny_updates": destiny_updates_count,
        "quasar_fixtures": quasar_count,
    }


def verify_destiny_golden_trajectories(fixture_path):
    """Differential verification of Destiny trajectories and wire packets (plan:T-11)."""
    import math
    data = json.load(open(fixture_path, "r"))
    scenarios = data.get("scenarios", [])
    if len(scenarios) != 6:
        raise AssertionError(f"Expected 6 scenarios in destiny suite, found {len(scenarios)}")

    verified = 0
    for sc in scenarios:
        # 1. Verify wire packet decoding
        raw_bytes = bytes.fromhex(sc["wire_packet_hex"])
        stamp, balls = decode_add_balls(raw_bytes)
        if stamp != 0 or len(balls) != 1:
            raise AssertionError(f"Wire packet decode error in scenario {sc['name']}")
        b = balls[0]
        if b["id"] != 1000001:
            raise AssertionError(f"Ball ID mismatch in scenario {sc['name']}")

        # 2. Verify checkpoints
        cps = sc["checkpoints"]
        if len(cps) < 2:
            raise AssertionError(f"Insufficient checkpoints in scenario {sc['name']}")

        mode = sc["mode_name"]
        if mode == "Stop":
            # Speed must strictly decrease
            for i in range(len(cps) - 1):
                v_curr = math.sqrt(sum(x * x for x in cps[i]["velocity"]))
                v_next = math.sqrt(sum(x * x for x in cps[i + 1]["velocity"]))
                if v_next >= v_curr:
                    raise AssertionError(f"Stop mode failed deceleration: {v_next} >= {v_curr}")
        elif mode == "Goto":
            # Speed must strictly increase
            for i in range(len(cps) - 1):
                v_curr = math.sqrt(sum(x * x for x in cps[i]["velocity"]))
                v_next = math.sqrt(sum(x * x for x in cps[i + 1]["velocity"]))
                if v_next <= v_curr:
                    raise AssertionError(f"Goto mode failed acceleration: {v_next} <= {v_curr}")
        elif mode == "MicroJump":
            p0 = cps[0]["position"]
            p1 = cps[1]["position"]
            disp = math.sqrt(sum((p1[k] - p0[k]) ** 2 for k in range(3)))
            if abs(disp - 100000.0) > 1e-3:
                raise AssertionError(f"MicroJump displacement {disp} != 100,000m")

        verified += 1

    return {
        "ok": True,
        "verified_scenarios": verified,
    }


def verify_dogma_golden_fits(fixture_path):
    """Differential verification of Dogma modifier graph and fits (plan:T-12)."""
    import math
    data = json.load(open(fixture_path, "r"))

    # 1. Verify stacking penalty curve S(i) = exp(-(i/2.67)^2)
    factors = data.get("stacking_factors", [])
    for entry in factors:
        idx = entry["index"]
        expected = math.exp(-((idx / 2.67) ** 2))
        actual = entry["factor"]
        if abs(actual - expected) > 1e-9:
            raise AssertionError(f"Stacking factor mismatch at index {idx}: {actual} vs {expected}")

    # 2. Verify fits
    fits = data.get("fits", [])
    if len(fits) < 4:
        raise AssertionError(f"Expected at least 4 canonical fits, found {len(fits)}")

    for fit in fits:
        attrs = fit.get("expected_attributes", {})
        if not attrs:
            raise AssertionError(f"Fit {fit['name']} has no expected_attributes")
        for attr_name, val in attrs.items():
            if not isinstance(val, (int, float)) or math.isnan(val):
                raise AssertionError(f"Fit {fit['name']} attribute {attr_name} has invalid value {val}")

    return {
        "ok": True,
        "verified_fits": len(fits),
        "verified_stacking_tiers": len(factors),
    }
