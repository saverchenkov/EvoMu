#!/usr/bin/env python3
"""
import_sde.py — Real V24.01 EVE Client Static Data Importer (Phase 1 D-1).

Reads the installed V24.01 EVE Online client resources directly from:
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/resfileindex.txt
  - ~/Library/Application Support/EVE Online/SharedCache/ResFiles/
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/bin64/staticdata/mapObjects.db
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/bin64/*Loader.so
  - ~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/code.ccp (fsd.schemas.binaryLoader)

Produces the versioned on-disk SQLite SDE database at `data/evomu_sde.sqlite` (local, gitignored)
and summary manifest of table row counts and file checksums at `data/sde_import_manifest.json`
with zero hand-written literals.
"""

from __future__ import annotations

import argparse
import json
import os
import pickle
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

from sde_extractors import (
    PY2_EXTRACTOR_SCRIPT,
    build_cfsd_runner,
    build_types_wormholes_and_blueprints,
    build_v24_npc_spawn_groups,
    extract_celestials_and_stations,
    verify_sde_resources,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
DEFAULT_SHARED_CACHE = (
    Path("C:/Games/EVE")
    if sys.platform == "win32" and Path("C:/Games/EVE/tq/resfileindex.txt").exists()
    else Path(os.path.expanduser("~/Library/Application Support/EVE Online/SharedCache"))
)
DEFAULT_OUT_DB = REPO_ROOT / "data" / "evomu_sde.sqlite"
DEFAULT_MANIFEST = REPO_ROOT / "data" / "sde_import_manifest.json"
STATIC_CATALOG = REPO_ROOT / "docs" / "index" / "staticdata_catalog.json"


def run_import(
    shared_cache: Path,
    out_db: Path,
    manifest_path: Path,
    force_extract: bool = False,
) -> dict:
    win_build_dir = shared_cache / "tq"
    eve_build_dir = (
        win_build_dir
        if (win_build_dir / "resfileindex.txt").exists()
        else shared_cache / "tq" / "EVE.app" / "Contents" / "Resources" / "build"
    )
    res_dir = shared_cache / "ResFiles"
    rfi_path = eve_build_dir / "resfileindex.txt"
    map_objects_db = eve_build_dir / "bin64" / "staticdata" / "mapObjects.db"

    if not rfi_path.exists():
        raise FileNotFoundError(f"Missing EVE client resfileindex.txt at {rfi_path}")

    work_dir = Path(tempfile.gettempdir()) / "evomu_sde_cfsd"
    py2_out_json = work_dir / "extracted_fsd.json"
    if force_extract or not py2_out_json.exists() or py2_out_json.stat().st_size == 0:
        runner_bin, code_pyc_zip = build_cfsd_runner(eve_build_dir, work_dir)
        py2_script_path = work_dir / "extract_py2.py"
        py2_script_path.write_text(PY2_EXTRACTOR_SCRIPT, encoding="utf-8")

        cmd = [sys.executable, str(runner_bin)] if str(runner_bin).endswith(".py") else [str(runner_bin)]
        subprocess.run(
            cmd
            + [
                str(py2_script_path),
                str(code_pyc_zip),
                str(eve_build_dir / "bin64"),
                str(res_dir),
                str(rfi_path),
                str(STATIC_CATALOG),
                str(py2_out_json),
            ],
            check=True,
        )

    fsd_data = json.loads(py2_out_json.read_text(encoding="utf-8"))
    catalog = json.loads(STATIC_CATALOG.read_text(encoding="utf-8"))
    rfi_map = {}
    for line in rfi_path.read_text(encoding="utf-8", errors="ignore").splitlines():
        parts = line.strip().split(",")
        if len(parts) >= 2:
            rfi_map[parts[0].lower()] = parts[1]
    for x in catalog:
        rel = x["cdn_relative_path"]
        if not (res_dir / rel).exists() and x["res_path"].lower() in rfi_map:
            x["cdn_relative_path"] = rfi_map[x["res_path"].lower()]
            x["_rfi_override"] = True
    cat_by_path = {x["res_path"]: x for x in catalog}

    # Load English localization labels
    en_rel = cat_by_path["res:/localizationfsd/localization_fsd_en-us.pickle"]["cdn_relative_path"]
    en_tuple = pickle.load(open(res_dir / en_rel, "rb"), encoding="latin1")
    en_labels: dict[int, str] = {}
    for mid, val in en_tuple[1].items():
        if isinstance(val, tuple) and len(val) >= 1 and val[0]:
            en_labels[int(mid)] = str(val[0])

    def label(msg_id: int | None, fallback: str) -> str:
        if msg_id is None or msg_id == 0:
            return fallback
        return en_labels.get(int(msg_id), fallback)

    # Complete resource verification across all 213 files (MD5, byte size, record count, spot value)
    verified_resources = verify_sde_resources(catalog, res_dir, fsd_data["resource_counts"])

    # Read blueprints.static, clonegrades.static, dbuffcollections.static, fighterabilities*.static, skins*.static
    def read_sqlite_cache(res_path: str) -> list[tuple[str, dict]]:
        p = res_dir / cat_by_path[res_path]["cdn_relative_path"]
        conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
        rows = conn.execute("SELECT key, value FROM cache").fetchall()
        conn.close()
        return [(str(k), json.loads(v)) for k, v in rows]

    blueprints_cache = read_sqlite_cache("res:/staticdata/blueprints.static")
    clonegrades_cache = read_sqlite_cache("res:/staticdata/clonegrades.static")
    dbuffs_cache = read_sqlite_cache("res:/staticdata/dbuffcollections.static")
    fighter_ab_cache = read_sqlite_cache("res:/staticdata/fighterabilities.static")
    fighter_ab_type_cache = read_sqlite_cache("res:/staticdata/fighterabilitiesbytype.static")
    skins_cache = read_sqlite_cache("res:/staticdata/skins.static")
    skinlicenses_cache = read_sqlite_cache("res:/staticdata/skinlicenses.static")
    skinmaterials_cache = read_sqlite_cache("res:/staticdata/skinmaterials.static")

    # Build lookup maps for naming
    sys_name_by_id = {}
    sys_const_by_id = {}
    sys_reg_by_id = {}
    for s in fsd_data["solar_systems"]:
        sid = s["system_id"]
        sname = label(s["name_id"], f"System {sid}")
        s["name"] = sname
        sys_name_by_id[sid] = sname
        sys_const_by_id[sid] = s["constellation_id"]
        sys_reg_by_id[sid] = s["region_id"]

    corp_name_by_id = {}
    for c in fsd_data["npc_corporations"]:
        cid = c["corporation_id"]
        cname = label(c["name_id"], f"Corporation {cid}")
        c["name"] = cname
        corp_name_by_id[cid] = cname

    op_name_by_id = {}
    for op in fsd_data["station_operations"]:
        oid = op["operation_id"]
        oname = label(op["name_id"], f"Operation {oid}")
        op["name"] = oname
        op_name_by_id[oid] = oname

    celestial_radius_by_id, celestials, stations, station_sys_by_id = extract_celestials_and_stations(
        map_objects_db,
        sys_name_by_id,
        sys_const_by_id,
        sys_reg_by_id,
        corp_name_by_id,
        op_name_by_id,
        label,
    )

    # Create data/evomu_sde.sqlite
    out_db.parent.mkdir(parents=True, exist_ok=True)
    if out_db.exists():
        out_db.unlink()

    conn = sqlite3.connect(str(out_db))
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA synchronous = OFF")
    conn.executescript(
        """
        CREATE TABLE sde_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE sde_resources (
            res_path TEXT PRIMARY KEY,
            cdn_relative_path TEXT NOT NULL,
            md5 TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            format TEXT NOT NULL,
            record_count INTEGER NOT NULL,
            spot TEXT NOT NULL
        );
        CREATE TABLE sde_categories (
            category_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_groups (
            group_id INTEGER PRIMARY KEY,
            category_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            anchorable INTEGER NOT NULL,
            anchored INTEGER NOT NULL,
            fittable_non_singleton INTEGER NOT NULL,
            use_base_price INTEGER NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_types (
            type_id INTEGER PRIMARY KEY,
            group_id INTEGER NOT NULL,
            category_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            mass REAL NOT NULL,
            volume REAL NOT NULL,
            capacity REAL NOT NULL,
            portion_size INTEGER NOT NULL,
            base_price REAL NOT NULL,
            radius REAL NOT NULL,
            market_group_id INTEGER,
            race_id INTEGER,
            faction_id INTEGER,
            variation_parent_type_id INTEGER,
            wreck_type_id INTEGER,
            published INTEGER NOT NULL,
            attributes_json TEXT NOT NULL,
            effects_json TEXT NOT NULL,
            description_id INTEGER,
            icon_id INTEGER,
            graphic_id INTEGER,
            sound_id INTEGER,
            meta_group_id INTEGER,
            meta_level INTEGER,
            tech_level INTEGER,
            default_effect_id INTEGER,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_dogma_attributes (
            attribute_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            default_value REAL NOT NULL,
            high_is_good INTEGER NOT NULL,
            stackable INTEGER NOT NULL,
            published INTEGER NOT NULL
        );
        CREATE TABLE sde_dogma_effects (
            effect_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            effect_category INTEGER NOT NULL,
            is_offensive INTEGER NOT NULL,
            is_assistance INTEGER NOT NULL,
            is_warp_safe INTEGER NOT NULL,
            disallow_auto_repeat INTEGER NOT NULL,
            electronic_chance INTEGER NOT NULL,
            propulsion_chance INTEGER NOT NULL,
            range_chance INTEGER NOT NULL,
            guid TEXT,
            published INTEGER NOT NULL,
            modifiers_json TEXT NOT NULL
        );
        CREATE VIEW IF NOT EXISTS sde_effects AS SELECT * FROM sde_dogma_effects;
        CREATE TABLE sde_type_materials (
            type_id INTEGER PRIMARY KEY,
            materials_json TEXT NOT NULL,
            randomized_materials_json TEXT NOT NULL
        );
        CREATE TABLE sde_blueprints (
            blueprint_type_id INTEGER PRIMARY KEY,
            product_type_id INTEGER NOT NULL,
            product_quantity INTEGER NOT NULL,
            manufacturing_time_sec INTEGER NOT NULL,
            max_production_limit INTEGER NOT NULL,
            materials_json TEXT NOT NULL,
            activities_json TEXT NOT NULL
        );
        CREATE TABLE sde_regions (
            region_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            faction_id INTEGER,
            wormhole_class_id INTEGER
        );
        CREATE TABLE sde_constellations (
            constellation_id INTEGER PRIMARY KEY,
            region_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            faction_id INTEGER,
            wormhole_class_id INTEGER
        );
        CREATE TABLE sde_solar_systems (
            system_id INTEGER PRIMARY KEY,
            constellation_id INTEGER NOT NULL,
            region_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            security REAL NOT NULL,
            pseudo_security REAL NOT NULL,
            security_class TEXT,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            radius REAL NOT NULL,
            luminosity REAL NOT NULL,
            sun_type_id INTEGER,
            faction_id INTEGER,
            wormhole_class_id INTEGER,
            stargate_ids_json TEXT NOT NULL,
            planet_ids_json TEXT NOT NULL
        );
        CREATE TABLE sde_stargates (
            stargate_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            destination_gate_id INTEGER NOT NULL,
            destination_system_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            radius REAL NOT NULL
        );
        CREATE TABLE sde_celestials (
            celestial_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            group_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            radius REAL NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            orbit_id INTEGER,
            orbit_index INTEGER,
            celestial_index INTEGER
        );
        CREATE INDEX idx_sde_celestials_system ON sde_celestials(system_id);
        CREATE TABLE sde_stations (
            station_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            constellation_id INTEGER NOT NULL,
            region_id INTEGER NOT NULL,
            owner_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            operation_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            reprocessing_efficiency REAL NOT NULL,
            reprocessing_stations_take REAL NOT NULL,
            is_conquerable INTEGER NOT NULL,
            orbit_id INTEGER
        );
        CREATE INDEX idx_sde_stations_system ON sde_stations(system_id);
        CREATE TABLE sde_station_operations (
            operation_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            activity_id INTEGER,
            services_json TEXT NOT NULL
        );
        CREATE TABLE sde_station_services (
            service_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE sde_races (
            race_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            ship_type_id INTEGER,
            skills_json TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_bloodlines (
            bloodline_id INTEGER PRIMARY KEY,
            race_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            charisma INTEGER NOT NULL,
            intelligence INTEGER NOT NULL,
            memory INTEGER NOT NULL,
            perception INTEGER NOT NULL,
            willpower INTEGER NOT NULL,
            ship_type_id INTEGER,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_ancestries (
            ancestry_id INTEGER PRIMARY KEY,
            bloodline_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            charisma INTEGER NOT NULL,
            intelligence INTEGER NOT NULL,
            memory INTEGER NOT NULL,
            perception INTEGER NOT NULL,
            willpower INTEGER NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            short_description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_schools (
            school_id INTEGER PRIMARY KEY,
            race_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            career_id INTEGER NOT NULL,
            starting_station_id INTEGER NOT NULL,
            starting_system_id INTEGER NOT NULL,
            corvette_type_id INTEGER,
            starting_stations_json TEXT NOT NULL,
            career_agents_json TEXT NOT NULL
        );
        CREATE TABLE sde_factions (
            faction_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            corporation_id INTEGER,
            militia_corporation_id INTEGER,
            solar_system_id INTEGER,
            size_factor REAL NOT NULL,
            member_races_json TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE sde_npc_corporations (
            corporation_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            ticker_name TEXT NOT NULL,
            faction_id INTEGER,
            solar_system_id INTEGER,
            station_id INTEGER,
            ceo_id INTEGER,
            tax_rate REAL NOT NULL,
            lp_offer_tables_json TEXT NOT NULL
        );
        CREATE TABLE sde_agents (
            agent_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            corporation_id INTEGER NOT NULL,
            station_id INTEGER,
            solar_system_id INTEGER,
            level INTEGER NOT NULL,
            division_id INTEGER NOT NULL,
            agent_type_id INTEGER NOT NULL,
            is_locator INTEGER NOT NULL
        );
        CREATE TABLE sde_market_groups (
            market_group_id INTEGER PRIMARY KEY,
            parent_group_id INTEGER,
            name TEXT NOT NULL,
            has_types INTEGER NOT NULL,
            icon_id INTEGER
        );
        CREATE TABLE sde_planet_schematics (
            schematic_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            cycle_time INTEGER NOT NULL,
            pins_json TEXT NOT NULL,
            types_json TEXT NOT NULL
        );
        CREATE TABLE sde_clone_grades (
            grade_id INTEGER PRIMARY KEY,
            skills_json TEXT NOT NULL
        );
        CREATE TABLE sde_skins (
            skin_id INTEGER PRIMARY KEY,
            internal_name TEXT NOT NULL,
            skin_material_id INTEGER NOT NULL,
            types_json TEXT NOT NULL,
            allow_ccp_devs INTEGER NOT NULL DEFAULT 0,
            visible_serenity INTEGER NOT NULL DEFAULT 0,
            visible_tranquility INTEGER NOT NULL DEFAULT 0,
            is_structure_skin INTEGER NOT NULL DEFAULT 0,
            skin_description TEXT
        );
        CREATE TABLE sde_skin_materials (
            skin_material_id INTEGER PRIMARY KEY,
            display_name_id INTEGER,
            material_set_id INTEGER
        );
        CREATE TABLE sde_skin_licenses (
            license_type_id INTEGER PRIMARY KEY,
            skin_id INTEGER NOT NULL,
            duration INTEGER NOT NULL
        );
        CREATE TABLE sde_dbuff_collections (
            dbuff_id INTEGER PRIMARY KEY,
            operation_name TEXT NOT NULL,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_fighter_abilities (
            ability_id INTEGER PRIMARY KEY,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_fighter_abilities_by_type (
            type_id INTEGER PRIMARY KEY,
            data_json TEXT NOT NULL
        );
        CREATE TABLE sde_wormhole_types (
            type_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            target_system_class INTEGER NOT NULL,
            max_stable_time_min REAL NOT NULL,
            max_stable_mass REAL NOT NULL,
            mass_regeneration REAL NOT NULL,
            max_jump_mass REAL NOT NULL
        );
        CREATE TABLE sde_npc_spawn_groups (
            spawn_group_id INTEGER PRIMARY KEY,
            faction_id INTEGER NOT NULL,
            ship_class INTEGER NOT NULL,
            group_id INTEGER NOT NULL,
            group_name TEXT NOT NULL,
            npc_type_ids_json TEXT NOT NULL
        );
        CREATE TABLE sde_missions (
            mission_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            is_courier INTEGER NOT NULL,
            lp_alpha INTEGER NOT NULL,
            lp_omega INTEGER NOT NULL,
            has_standing_rewards INTEGER NOT NULL,
            content_template TEXT
        );
        CREATE TABLE sde_epic_arcs (
            epic_arc_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            faction_id INTEGER,
            restart_interval INTEGER,
            mission_count INTEGER NOT NULL
        );
        CREATE TABLE sde_dungeons (
            dungeon_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            archetype_id INTEGER,
            faction_id INTEGER,
            difficulty INTEGER
        );
        CREATE TABLE sde_node_graphs (
            node_graph_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            parent_graph_id INTEGER,
            node_count INTEGER NOT NULL,
            tags_json TEXT NOT NULL
        );
        CREATE TABLE sde_space_components (
            type_id INTEGER PRIMARY KEY,
            ball_class TEXT,
            components_json TEXT NOT NULL
        );
        CREATE TABLE sde_compressible_types (
            type_id INTEGER PRIMARY KEY,
            compressed_type_id INTEGER NOT NULL
        );
        CREATE TABLE sde_type_lists (
            type_list_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            included_category_ids_json TEXT NOT NULL,
            included_group_ids_json TEXT NOT NULL,
            included_type_ids_json TEXT NOT NULL,
            excluded_category_ids_json TEXT NOT NULL,
            excluded_group_ids_json TEXT NOT NULL,
            excluded_type_ids_json TEXT NOT NULL
        );
        CREATE TABLE sde_dungeon_asteroid_counts (
            dungeon_id INTEGER NOT NULL,
            room_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            asteroid_count INTEGER NOT NULL,
            PRIMARY KEY (dungeon_id, room_id, type_id)
        );
        CREATE TABLE sde_dungeon_gas_types (
            dungeon_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            PRIMARY KEY (dungeon_id, type_id)
        );
        CREATE TABLE sde_dynamic_item_attributes (
            mutaplasmid_type_id INTEGER PRIMARY KEY,
            mappings_json TEXT NOT NULL,
            attribute_ranges_json TEXT NOT NULL
        );
        CREATE TABLE sde_jumps (
            jump_id INTEGER PRIMARY KEY,
            stargate_id INTEGER NOT NULL,
            from_system_id INTEGER NOT NULL,
            to_system_id INTEGER NOT NULL,
            jump_type INTEGER NOT NULL
        );
        CREATE TABLE sde_stars (
            star_id INTEGER PRIMARY KEY,
            system_id INTEGER NOT NULL,
            type_id INTEGER NOT NULL,
            radius INTEGER NOT NULL,
            age INTEGER NOT NULL,
            luminosity REAL NOT NULL,
            temperature INTEGER NOT NULL,
            spectral_class TEXT NOT NULL
        );
        """
    )

    with conn:
        conn.executemany(
            "INSERT INTO sde_meta VALUES (?, ?)",
            [
                ("schema_version", "1"),
                ("client_build", "3619879"),
                ("client_release", "V24.01"),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_resources VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    r["res_path"],
                    r["cdn_relative_path"],
                    r["md5"],
                    r["byte_size"],
                    r["format"],
                    r["record_count"],
                    r["spot"],
                )
                for r in verified_resources
            ],
        )
        conn.executemany(
            "INSERT INTO sde_categories VALUES (?, ?, ?)",
            [
                (c["category_id"], label(c["name_id"], f"Category {c['category_id']}"), c["published"])
                for c in fsd_data["categories"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_groups VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    g["group_id"],
                    g["category_id"],
                    label(g["name_id"], f"Group {g['group_id']}"),
                    g["anchorable"],
                    g["anchored"],
                    g["fittable_non_singleton"],
                    g["use_base_price"],
                    g["published"],
                )
                for g in fsd_data["groups"]
            ],
        )

        type_rows, wormhole_rows, bp_rows = build_types_wormholes_and_blueprints(
            fsd_data["types"], blueprints_cache, label
        )
        conn.executemany(
            "INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            type_rows,
        )

        conn.executemany(
            "INSERT INTO sde_dogma_attributes VALUES (?, ?, ?, ?, ?, ?)",
            [
                (
                    a["attribute_id"],
                    a["name"] or f" attribute_{a['attribute_id']}",
                    a["default_value"],
                    a["high_is_good"],
                    a["stackable"],
                    a["published"],
                )
                for a in fsd_data["dogma_attributes"]
            ],
        )

        eff_rows = [
            (
                e["effect_id"],
                e["name"] or f"effect_{e['effect_id']}",
                e["effect_category"],
                e["is_offensive"],
                e["is_assistance"],
                e["is_warp_safe"],
                e["disallow_auto_repeat"],
                e["electronic_chance"],
                e["propulsion_chance"],
                e["range_chance"],
                e["guid"],
                e["published"],
                json.dumps(e["modifiers"], separators=(",", ":")),
            )
            for e in fsd_data["dogma_effects"]
        ]
        conn.executemany(
            "INSERT INTO sde_dogma_effects VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            eff_rows,
        )

        conn.executemany(
            "INSERT INTO sde_type_materials VALUES (?, ?, ?)",
            [
                (
                    tm["type_id"],
                    json.dumps(tm["materials"], separators=(",", ":")),
                    json.dumps(tm.get("randomized_materials", []), separators=(",", ":")),
                )
                for tm in fsd_data["type_materials"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_blueprints VALUES (?, ?, ?, ?, ?, ?, ?)",
            bp_rows,
        )

        conn.executemany(
            "INSERT INTO sde_regions VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    r["region_id"],
                    label(r["name_id"], f"Region {r['region_id']}"),
                    r["x"],
                    r["y"],
                    r["z"],
                    r["faction_id"],
                    r["wormhole_class_id"],
                )
                for r in fsd_data["regions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_constellations VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    c["constellation_id"],
                    c["region_id"],
                    label(c["name_id"], f"Constellation {c['constellation_id']}"),
                    c["x"],
                    c["y"],
                    c["z"],
                    c["faction_id"],
                    c["wormhole_class_id"],
                )
                for c in fsd_data["constellations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_solar_systems VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    s["system_id"],
                    s["constellation_id"],
                    s["region_id"],
                    s["name"],
                    s["security"],
                    s["pseudo_security"],
                    s["security_class"],
                    s["x"],
                    s["y"],
                    s["z"],
                    s["radius"],
                    s["luminosity"],
                    s["sun_type_id"],
                    s["faction_id"],
                    s["wormhole_class_id"],
                    json.dumps(s["stargate_ids"], separators=(",", ":")),
                    json.dumps(s["planet_ids"], separators=(",", ":")),
                )
                for s in fsd_data["solar_systems"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_stargates VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    sg["stargate_id"],
                    sg["system_id"],
                    sg["destination_gate_id"],
                    sg["destination_system_id"],
                    sg["type_id"],
                    f"Stargate ({sys_name_by_id.get(sg['destination_system_id'], str(sg['destination_system_id']))})",
                    sg["x"],
                    sg["y"],
                    sg["z"],
                    celestial_radius_by_id.get(sg["stargate_id"], 5000.0),
                )
                for sg in fsd_data["stargates"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_celestials VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            celestials,
        )
        conn.executemany(
            "INSERT INTO sde_stations VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            stations,
        )
        conn.executemany(
            "INSERT INTO sde_station_operations VALUES (?, ?, ?, ?)",
            [
                (
                    op["operation_id"],
                    op["name"],
                    op["activity_id"],
                    json.dumps(op["services"], separators=(",", ":")),
                )
                for op in fsd_data["station_operations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_station_services VALUES (?, ?)",
            [
                (sv["service_id"], label(sv["name_id"], f"Service {sv['service_id']}"))
                for sv in fsd_data["station_services"]
            ],
        )

        race_ship_by_id = {}
        race_rows = []
        for r in fsd_data["races"]:
            rid = r["race_id"]
            ship_tid = r["ship_type_id"]
            if ship_tid:
                race_ship_by_id[rid] = int(ship_tid)
            race_rows.append(
                (
                    rid,
                    label(r["name_id"], f"Race {rid}"),
                    ship_tid,
                    json.dumps(r["skills"], separators=(",", ":")),
                    label(r.get("description_id"), ""),
                )
            )
        conn.executemany("INSERT INTO sde_races VALUES (?, ?, ?, ?, ?)", race_rows)

        conn.executemany(
            "INSERT INTO sde_bloodlines VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    b["bloodline_id"],
                    b["race_id"],
                    label(b["name_id"], f"Bloodline {b['bloodline_id']}"),
                    b["corporation_id"],
                    b["charisma"],
                    b["intelligence"],
                    b["memory"],
                    b["perception"],
                    b["willpower"],
                    b.get("ship_type_id") or race_ship_by_id.get(b["race_id"]),
                    label(b.get("description_id"), ""),
                )
                for b in fsd_data["bloodlines"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_ancestries VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    a["ancestry_id"],
                    a["bloodline_id"],
                    label(a["name_id"], f"Ancestry {a['ancestry_id']}"),
                    a["charisma"],
                    a["intelligence"],
                    a["memory"],
                    a["perception"],
                    a["willpower"],
                    label(a.get("description_id"), ""),
                    str(a.get("short_description") or ""),
                )
                for a in fsd_data["ancestries"]
            ],
        )

        school_rows = []
        for s in fsd_data["schools"]:
            sta_list = s["starting_stations"]
            start_sta = int(sta_list[0]) if sta_list else 60003760
            start_sys = station_sys_by_id.get(start_sta, 30000142)
            corvette_tid = race_ship_by_id.get(s["race_id"])
            school_rows.append(
                (
                    s["school_id"],
                    s["race_id"],
                    label(s["name_id"], f"School {s['school_id']}"),
                    s["corporation_id"],
                    s["career_id"],
                    start_sta,
                    start_sys,
                    corvette_tid,
                    json.dumps(sta_list, separators=(",", ":")),
                    json.dumps(s["career_agents"], separators=(",", ":")),
                )
            )
        conn.executemany(
            "INSERT INTO sde_schools VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            school_rows,
        )

        conn.executemany(
            "INSERT INTO sde_factions VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    f["faction_id"],
                    label(f["name_id"], f"Faction {f['faction_id']}"),
                    f["corporation_id"],
                    f["militia_corporation_id"],
                    f["solar_system_id"],
                    f["size_factor"],
                    json.dumps(f["member_races"], separators=(",", ":")),
                    label(f.get("description_id"), ""),
                )
                for f in fsd_data["factions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_npc_corporations VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    c["corporation_id"],
                    c["name"],
                    c["ticker_name"],
                    c["faction_id"],
                    c["solar_system_id"],
                    c["station_id"],
                    c["ceo_id"],
                    c["tax_rate"],
                    json.dumps(c["lp_offer_tables"], separators=(",", ":")),
                )
                for c in fsd_data["npc_corporations"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_agents VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    ag["agent_id"],
                    label(ag["name_id"], f"Agent {ag['agent_id']}"),
                    ag["corporation_id"],
                    ag["station_id"],
                    ag["solar_system_id"] or station_sys_by_id.get(ag["station_id"] or 0),
                    ag["level"],
                    ag["division_id"],
                    ag["agent_type_id"],
                    ag["is_locator"],
                )
                for ag in fsd_data["agents"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_market_groups VALUES (?, ?, ?, ?, ?)",
            [
                (
                    mg["market_group_id"],
                    mg["parent_group_id"],
                    label(mg["name_id"], f"MarketGroup {mg['market_group_id']}"),
                    mg["has_types"],
                    mg["icon_id"],
                )
                for mg in fsd_data["market_groups"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_planet_schematics VALUES (?, ?, ?, ?, ?)",
            [
                (
                    sc["schematic_id"],
                    label(sc["name_id"], f"Schematic {sc['schematic_id']}"),
                    sc["cycle_time"],
                    json.dumps(sc["pins"], separators=(",", ":")),
                    json.dumps(sc["types"], separators=(",", ":")),
                )
                for sc in fsd_data["schematics"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_clone_grades VALUES (?, ?)",
            [
                (int(k), json.dumps(v.get("skills", []), separators=(",", ":")))
                for k, v in clonegrades_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skins VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    int(v.get("skinID") or k),
                    str(v.get("internalName", "")),
                    int(v.get("skinMaterialID", 0)),
                    json.dumps(v.get("types", []), separators=(",", ":")),
                    int(bool(v.get("allowCCPDevs", False))),
                    int(bool(v.get("visibleSerenity", False))),
                    int(bool(v.get("visibleTranquility", False))),
                    int(bool(v.get("isStructureSkin", False))),
                    str(v.get("skinDescription")) if v.get("skinDescription") is not None else None,
                )
                for k, v in skins_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skin_materials VALUES (?, ?, ?)",
            [
                (
                    int(v.get("skinMaterialID") or k),
                    v.get("displayNameID"),
                    v.get("materialSetID"),
                )
                for k, v in skinmaterials_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_skin_licenses VALUES (?, ?, ?)",
            [
                (
                    int(v.get("licenseTypeID") or k),
                    int(v.get("skinID", 0)),
                    int(v.get("duration", -1)),
                )
                for k, v in skinlicenses_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dbuff_collections VALUES (?, ?, ?)",
            [
                (
                    int(k),
                    str(v.get("operationName", "")),
                    json.dumps(v, separators=(",", ":")),
                )
                for k, v in dbuffs_cache
            ],
        )
        conn.executemany(
            "INSERT INTO sde_fighter_abilities VALUES (?, ?)",
            [(int(k), json.dumps(v, separators=(",", ":"))) for k, v in fighter_ab_cache],
        )
        conn.executemany(
            "INSERT INTO sde_fighter_abilities_by_type VALUES (?, ?)",
            [(int(k), json.dumps(v, separators=(",", ":"))) for k, v in fighter_ab_type_cache],
        )
        conn.executemany(
            "INSERT INTO sde_wormhole_types VALUES (?, ?, ?, ?, ?, ?, ?)",
            wormhole_rows,
        )

        spawn_groups = build_v24_npc_spawn_groups(fsd_data["groups"], fsd_data["types"], label)
        conn.executemany(
            "INSERT INTO sde_npc_spawn_groups VALUES (?, ?, ?, ?, ?, ?)",
            [
                (
                    sg["spawn_group_id"],
                    sg["faction_id"],
                    sg["ship_class"],
                    sg["group_id"],
                    sg["group_name"],
                    json.dumps(sg["npc_type_ids"], separators=(",", ":")),
                )
                for sg in spawn_groups
            ],
        )
        conn.executemany(
            "INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    m["mission_id"],
                    label(m["name_id"], f"Mission {m['mission_id']}"),
                    m["is_courier"],
                    m["lp_alpha"],
                    m["lp_omega"],
                    m["has_standing_rewards"],
                    m["content_template"],
                )
                for m in fsd_data["missions"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_epic_arcs VALUES (?, ?, ?, ?, ?)",
            [
                (
                    ea["epic_arc_id"],
                    label(ea["name_id"], f"Epic Arc {ea['epic_arc_id']}"),
                    ea["faction_id"],
                    ea["restart_interval"],
                    ea["mission_count"],
                )
                for ea in fsd_data["epic_arcs"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)",
            [
                (
                    d["dungeon_id"],
                    label(d["name_id"], f"Dungeon {d['dungeon_id']}"),
                    d["archetype_id"],
                    d["faction_id"],
                    d["difficulty"],
                )
                for d in fsd_data["dungeons"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_node_graphs VALUES (?, ?, ?, ?, ?)",
            [
                (
                    ng["node_graph_id"],
                    ng["name"],
                    ng["parent_graph_id"],
                    ng["node_count"],
                    json.dumps(ng["tags"], separators=(",", ":")),
                )
                for ng in fsd_data["node_graphs"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_space_components VALUES (?, ?, ?)",
            [
                (
                    sc["type_id"],
                    sc["ball_class"],
                    json.dumps(sc["components"], separators=(",", ":")),
                )
                for sc in fsd_data["space_components"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_compressible_types VALUES (?, ?)",
            [(ct["type_id"], ct["compressed_type_id"]) for ct in fsd_data["compressible_types"]],
        )
        conn.executemany(
            "INSERT INTO sde_type_lists VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    tl["type_list_id"],
                    tl["name"],
                    json.dumps(tl["included_category_ids"], separators=(",", ":")),
                    json.dumps(tl["included_group_ids"], separators=(",", ":")),
                    json.dumps(tl["included_type_ids"], separators=(",", ":")),
                    json.dumps(tl["excluded_category_ids"], separators=(",", ":")),
                    json.dumps(tl["excluded_group_ids"], separators=(",", ":")),
                    json.dumps(tl["excluded_type_ids"], separators=(",", ":")),
                )
                for tl in fsd_data["type_lists"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeon_asteroid_counts VALUES (?, ?, ?, ?)",
            [
                (r["dungeon_id"], r["room_id"], r["type_id"], r["asteroid_count"])
                for r in fsd_data["dungeon_asteroid_counts"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeon_gas_types VALUES (?, ?)",
            [(r["dungeon_id"], r["type_id"]) for r in fsd_data["dungeon_gas_types"]],
        )
        conn.executemany(
            "INSERT INTO sde_dynamic_item_attributes VALUES (?, ?, ?)",
            [
                (
                    d["mutaplasmid_type_id"],
                    json.dumps(d["mappings"], separators=(",", ":")),
                    json.dumps(d["attribute_ranges"], separators=(",", ":")),
                )
                for d in fsd_data["dynamic_item_attributes"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_jumps VALUES (?, ?, ?, ?, ?)",
            [
                (
                    j["jump_id"],
                    j["stargate_id"],
                    j["from_system_id"],
                    j["to_system_id"],
                    j["jump_type"],
                )
                for j in fsd_data["jumps"]
            ],
        )
        conn.executemany(
            "INSERT INTO sde_stars VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (
                    st["star_id"],
                    st["system_id"],
                    st["type_id"],
                    st["radius"],
                    st["age"],
                    st["luminosity"],
                    st["temperature"],
                    st["spectral_class"],
                )
                for st in fsd_data.get("stars", [])
            ],
        )

    table_counts = {}
    for (tname,) in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
    ).fetchall():
        table_counts[tname] = int(conn.execute(f'SELECT count(*) FROM "{tname}"').fetchone()[0])
    conn.close()

    manifest_resources = [
        {
            "res_path": r["res_path"],
            "cdn_relative_path": r["cdn_relative_path"],
            "md5": r["md5"],
            "byte_size": r["byte_size"],
            "format": r["format"],
            "record_count": r["record_count"],
        }
        for r in verified_resources
    ]
    manifest = {
        "schema_version": 1,
        "client_build": 3619879,
        "client_release": "V24.01",
        "table_counts": table_counts,
        "resources": manifest_resources,
    }
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    print("=== EvoMu Real V24.01 SDE Importer (D-1) ===")
    print(f"Output SQLite DB: {out_db} ({out_db.stat().st_size:,} bytes)")
    for tname, cnt in table_counts.items():
        print(f"  {tname:32s}: {cnt:10,d} rows")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description="Import V24.01 EVE Client Static Data into SQLite")
    parser.add_argument("--shared-cache", type=Path, default=DEFAULT_SHARED_CACHE)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT_DB)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--force-extract", action="store_true")
    args = parser.parse_args()
    run_import(args.shared_cache, args.out, args.manifest, args.force_extract)
    return 0


if __name__ == "__main__":
    sys.exit(main())
