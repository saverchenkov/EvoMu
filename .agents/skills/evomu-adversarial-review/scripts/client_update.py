#!/usr/bin/env python3
"""Detect EVE Online SharedCache client updates, classify impact tier, and sync staticdata_catalog.json.

Usage:
  python3 .agents/skills/evomu-adversarial-review/scripts/client_update.py [--json]
  python3 .agents/skills/evomu-adversarial-review/scripts/client_update.py --sync-catalog <path/to/staticdata_catalog.json>
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import pickle
import re
import sqlite3
import struct
import sys
import zipfile
import zlib

REPO_ROOT = pathlib.Path(__file__).resolve().parents[4]
DEFAULT_SHARED_CACHE = (
    pathlib.Path("C:/Games/EVE")
    if sys.platform == "win32" and pathlib.Path("C:/Games/EVE/tq/resfileindex.txt").exists()
    else pathlib.Path.home() / "Library/Application Support/EVE Online/SharedCache"
)


def find_build_dir(shared_cache: pathlib.Path) -> pathlib.Path | None:
    for cand in (
        shared_cache / "tq" / "EVE.app" / "Contents" / "Resources" / "build",
        shared_cache / "tq",
    ):
        if (cand / "start.ini").is_file():
            return cand
    return None


def parse_start_ini(ini_path: pathlib.Path) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in ini_path.read_text(encoding="utf-8", errors="ignore").splitlines():
        line = line.strip()
        if "=" in line and not line.startswith("[") and not line.startswith(";"):
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def extract_macho_version(code_ccp: pathlib.Path) -> int | None:
    if not code_ccp.is_file():
        return None
    with zipfile.ZipFile(code_ccp) as zf:
        target = "eve/common/script/net/eveMachoNetVersion.pyj"
        if target not in zf.namelist():
            cands = [n for n in zf.namelist() if n.endswith("MachoNetVersion.pyj")]
            if not cands:
                return None
            target = cands[0]
        raw = zf.read(target)
        try:
            raw = zlib.decompress(raw)
        except zlib.error:
            pass
        # Python 2.7 marshal TYPE_INT ('i') followed by 32-bit little-endian integer in reasonable machoVersion range
        for m in re.finditer(rb"i(....)", raw, flags=re.DOTALL):
            val = struct.unpack("<i", m.group(1))[0]
            if 100 <= val <= 10000:
                return val
    return None


def read_server_pins(repo: pathlib.Path) -> dict:
    lib_rs = (repo / "crates" / "eve-machonet" / "src" / "lib.rs").read_text(encoding="utf-8")
    macho_m = re.search(r"V24_MACHO_VERSION:\s*u32\s*=\s*([0-9_]+)", lib_rs)
    build_m = re.search(r"V24_BOOT_BUILD:\s*u32\s*=\s*([0-9_]+)", lib_rs)
    code_m = re.search(r'V24_CODENAME_AT_REGION:\s*&str\s*=\s*"([^"]+)"', lib_rs)
    ver_const_m = re.search(r"V24_VERSION_NUMBER:\s*f64\s*=\s*([0-9.]+)", lib_rs)
    ver_field_m = re.search(r"version_number:\s*([0-9.]+)", lib_rs)
    version = float(ver_const_m.group(1) if ver_const_m else (ver_field_m.group(1) if ver_field_m else "0"))
    codename_at_region = code_m.group(1) if code_m else ""
    codename = codename_at_region.split("@", 1)[0]
    readme_text = (repo / "README.md").read_text(encoding="utf-8") if (repo / "README.md").is_file() else ""
    readme_m = re.search(r"(V\d+\.\d+)\s+client,?\s+\(?build\s+(\d+)\)?", readme_text)
    return {
        "macho_version": int(macho_m.group(1).replace("_", "")) if macho_m else None,
        "build": int(build_m.group(1).replace("_", "")) if build_m else None,
        "version": version,
        "codename": codename,
        "codename_at_region": codename_at_region,
        "readme_codename": readme_m.group(1) if readme_m else None,
        "readme_build": int(readme_m.group(2)) if readme_m else None,
    }


def inspect_update(repo: pathlib.Path, shared_cache: pathlib.Path) -> dict:
    server = read_server_pins(repo)
    build_dir = find_build_dir(shared_cache)
    if build_dir is None:
        return {"status": "NO_CLIENT", "server": server}

    ini = parse_start_ini(build_dir / "start.ini")
    code_ccp = build_dir / "code.ccp"
    client_macho = extract_macho_version(code_ccp)
    client_ver = float(ini.get("version", "0") or "0")
    client_build = int(ini.get("build", "0") or "0")
    client_codename = ini.get("codename", "")
    client_region = ini.get("region", "ccp")
    client_codename_at_region = f"{client_codename}@{client_region}"

    # Quasar check
    q_manifest_path = repo / "data" / "quasar_descriptor_manifest.json"
    q_manifest = json.loads(q_manifest_path.read_text(encoding="utf-8")) if q_manifest_path.is_file() else {}
    pb2_files: list[str] = []
    if code_ccp.is_file():
        with zipfile.ZipFile(code_ccp) as zf:
            pb2_files = sorted(
                n for n in zf.namelist() if n.startswith("eveProto/generated/") and n.endswith("_pb2.pyj")
            )

    # SDE / staticdata check
    rfi_path = build_dir / "resfileindex.txt"
    rfi_static: dict[str, dict] = {}
    if rfi_path.is_file():
        for line in rfi_path.read_text(encoding="utf-8", errors="ignore").splitlines():
            parts = line.strip().split(",")
            if len(parts) >= 4:
                rp = parts[0].lower()
                if rp.startswith("res:/staticdata/") or rp.startswith("res:/localizationfsd/"):
                    rfi_static[rp] = {"res_path": parts[0], "cdn_relative_path": parts[1], "md5": parts[2], "byte_size": int(parts[3])}

    cat_path = repo / "docs" / "index" / "staticdata_catalog.json"
    cat_list = json.loads(cat_path.read_text(encoding="utf-8")) if cat_path.is_file() else []
    cat_map = {x["res_path"].lower(): x for x in cat_list}
    sde_changed = sorted(rp for rp, x in cat_map.items() if rp in rfi_static and rfi_static[rp]["md5"] != x["md5"])
    sde_new = sorted(set(rfi_static) - set(cat_map))
    sde_removed = sorted(set(cat_map) - set(rfi_static))

    sde_manifest_path = repo / "data" / "sde_import_manifest.json"
    sde_manifest = json.loads(sde_manifest_path.read_text(encoding="utf-8")) if sde_manifest_path.is_file() else {}

    # Destiny check
    carbon_json = build_dir / "carbon.json"
    client_carbon_destiny = None
    if carbon_json.is_file():
        try:
            client_carbon_destiny = json.loads(carbon_json.read_text(encoding="utf-8")).get("libraries", {}).get("carbon_destiny")
        except Exception:  # noqa: BLE001
            pass
    gv_path = repo / "crates" / "eve-destiny" / "tests" / "fixtures" / "ccp_golden_vectors.json"
    pinned_carbon_destiny = None
    if gv_path.is_file():
        gv = json.loads(gv_path.read_text(encoding="utf-8"))
        pinned_carbon_destiny = gv.get("versions", {}).get(gv.get("active_version", ""), {}).get("client_meta", {}).get("carbon_destiny")

    macho_changed = client_macho is not None and server["macho_version"] != client_macho
    destiny_lib_changed = bool(client_carbon_destiny and pinned_carbon_destiny and client_carbon_destiny != pinned_carbon_destiny)
    release_changed = (
        abs(client_ver - (server["version"] or 0.0)) > 1e-4
        or client_codename_at_region != server["codename_at_region"]
    )
    build_changed = client_build != server["build"]
    readme_outdated = server["readme_codename"] != client_codename or server["readme_build"] != client_build
    quasar_count_changed = bool(pb2_files) and len(pb2_files) != q_manifest.get("descriptor_file_count")
    sde_outdated = bool(sde_changed or sde_new or sde_removed or sde_manifest.get("client_build") != client_build)

    if macho_changed or destiny_lib_changed or sde_removed:
        tier = "TIER_3_PROTOCOL_OR_RUNTIME_CHANGE"
    elif release_changed:
        tier = "TIER_2_POINT_RELEASE"
    elif build_changed or readme_outdated or quasar_count_changed or sde_outdated:
        tier = "TIER_1_BUILD_OR_DATA_BUMP"
    else:
        tier = "UP_TO_DATE"

    return {
        "tier": tier,
        "client": {
            "codename": client_codename,
            "codename_at_region": client_codename_at_region,
            "version": client_ver,
            "build": client_build,
            "branch": ini.get("branch", ""),
            "macho_version": client_macho,
            "carbon_destiny": client_carbon_destiny,
            "quasar_pb2_count": len(pb2_files),
            "static_resource_count": len(rfi_static),
        },
        "server": {
            **server,
            "quasar_manifest_file_count": q_manifest.get("descriptor_file_count"),
            "sde_manifest_build": sde_manifest.get("client_build"),
            "sde_manifest_release": sde_manifest.get("client_release"),
            "static_catalog_count": len(cat_list),
            "pinned_carbon_destiny": pinned_carbon_destiny,
        },
        "deltas": {
            "handshake_blocks_new_client": release_changed or macho_changed or (server["build"] or 0) > client_build,
            "macho_changed": macho_changed,
            "release_changed": release_changed,
            "build_changed": build_changed,
            "quasar_file_delta": len(pb2_files) - (q_manifest.get("descriptor_file_count") or 0),
            "sde_changed_count": len(sde_changed),
            "sde_new_resources": [rfi_static[k]["res_path"] for k in sde_new],
            "sde_removed_resources": sde_removed,
            "destiny_lib_changed": destiny_lib_changed,
        },
    }


def detect_resource_format(res_file: pathlib.Path, res_path: str) -> tuple[str, int | None]:
    if not res_file.is_file():
        return ("Unknown", None)
    head = res_file.read_bytes()[:32]
    if head.startswith(b"SQLite format 3"):
        conn = sqlite3.connect(f"file:{res_file}?immutable=1", uri=True)
        try:
            tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()]
            total = sum(conn.execute(f'SELECT COUNT(*) FROM "{t}"').fetchone()[0] for t in tables)
            return ("SQLite 3 (.static)", total)
        finally:
            conn.close()
    if res_path.endswith(".pickle"):
        obj = pickle.load(open(res_file, "rb"), encoding="latin1")
        if isinstance(obj, tuple) and len(obj) >= 2 and isinstance(obj[1], dict):
            return ("Pickle (protocol 0)", len(obj[1]))
        if isinstance(obj, dict):
            return ("Pickle", len(obj))
        return ("Pickle", None)
    if res_path.endswith(".fsdbinary"):
        return ("FSD Binary", None)
    return ("Binary", None)


def sync_staticdata_catalog(shared_cache: pathlib.Path, catalog_path: pathlib.Path) -> int:
    build_dir = find_build_dir(shared_cache)
    if build_dir is None:
        raise FileNotFoundError(f"No EVE client build found under {shared_cache}")
    rfi_path = build_dir / "resfileindex.txt"
    res_dir = shared_cache / "ResFiles"
    existing = json.loads(catalog_path.read_text(encoding="utf-8")) if catalog_path.is_file() else []
    by_lower = {x["res_path"].lower(): dict(x) for x in existing}

    rfi_entries: dict[str, tuple[str, str, str, int]] = {}
    for line in rfi_path.read_text(encoding="utf-8", errors="ignore").splitlines():
        parts = line.strip().split(",")
        if len(parts) >= 4:
            rp_low = parts[0].lower()
            if rp_low.startswith("res:/staticdata/") or rp_low.startswith("res:/localizationfsd/"):
                rfi_entries[rp_low] = (parts[0], parts[1], parts[2], int(parts[3]))

    updated: list[dict] = []
    for rp_low in sorted(rfi_entries):
        orig_rp, rel, md5, sz = rfi_entries[rp_low]
        prev = by_lower.get(rp_low)
        if prev and prev.get("md5") == md5 and prev.get("cdn_relative_path") == rel:
            updated.append(prev)
            continue
        fmt, rec_cnt = detect_resource_format(res_dir / rel, rp_low)
        entry = dict(prev) if prev else {"res_path": orig_rp, "format": fmt}
        entry["res_path"] = prev["res_path"] if prev else orig_rp
        entry["cdn_relative_path"] = rel
        entry["md5"] = md5
        entry["byte_size"] = sz
        if prev and "format" in prev:
            entry["format"] = prev["format"]
        if rec_cnt is not None:
            entry["record_count"] = rec_cnt
        updated.append(entry)

    updated.sort(key=lambda x: x["res_path"])
    catalog_path.write_text(json.dumps(updated, indent=2) + "\n", encoding="utf-8")
    return len(updated)


def main() -> int:
    ap = argparse.ArgumentParser(description="Inspect EVE SharedCache client updates against EvoMu server pins.")
    ap.add_argument("--shared-cache", type=pathlib.Path, default=DEFAULT_SHARED_CACHE)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--sync-catalog", type=pathlib.Path, help="Update staticdata_catalog.json in place from resfileindex.txt")
    args = ap.parse_args()

    if args.sync_catalog:
        n = sync_staticdata_catalog(args.shared_cache, args.sync_catalog)
        print(f"[OK] Synced {n} staticdata/localization resources into {args.sync_catalog}")
        return 0

    report = inspect_update(REPO_ROOT, args.shared_cache)
    if args.json:
        print(json.dumps(report, indent=2))
        return 0

    if report.get("status") == "NO_CLIENT":
        print("CLIENT_UPDATE: no SharedCache EVE client found; skipping.")
        return 0

    c, s, d = report["client"], report["server"], report["deltas"]
    print(f"=== EvoMu Client Update Check: {report['tier']} ===")
    print(f"  Installed Client : {c['codename_at_region']} | version={c['version']} | build={c['build']} | machoVersion={c['macho_version']}")
    print(f"  Server Handshake : {s['codename_at_region']} | version={s['version']} | build={s['build']} | machoVersion={s['macho_version']}")
    print(f"  README.md Pin    : {s['readme_codename']} (build {s['readme_build']})")
    print(f"  Quasar Protobufs : client={c['quasar_pb2_count']} files vs manifest={s['quasar_manifest_file_count']} (delta={d['quasar_file_delta']:+d})")
    print(f"  SDE Resources    : client={c['static_resource_count']} vs catalog={s['static_catalog_count']} "
          f"(changed={d['sde_changed_count']}, new={len(d['sde_new_resources'])}, removed={len(d['sde_removed_resources'])}; "
          f"manifest={s['sde_manifest_release']}/{s['sde_manifest_build']})")
    print(f"  Destiny Runtime  : client={c['carbon_destiny']} | changed={d['destiny_lib_changed']}")
    if d["sde_new_resources"]:
        print(f"  New SDE files    : {', '.join(d['sde_new_resources'])}")
    if d["handshake_blocks_new_client"]:
        print("  [P0] New client handshake is BLOCKED until Track 1 (docs/CLIENT_UPDATE_PROTOCOL.md) lands on main!")
    return 0 if report["tier"] == "UP_TO_DATE" else 2


if __name__ == "__main__":
    sys.exit(main())
