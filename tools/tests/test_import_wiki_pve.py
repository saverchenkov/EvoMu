"""Unit tests for tools/import_wiki_pve.py using synthetic in-memory/temp fixtures."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import re
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import import_wiki_pve as imp  # noqa: E402


def _build_synthetic_sde(db_path: Path, mission_ext_path: Path) -> None:
    """Create a minimal synthetic SDE SQLite DB and mission_ext JSON for isolated tests."""
    conn = sqlite3.connect(str(db_path))
    conn.executescript(
        """
        CREATE TABLE sde_groups (
            group_id INTEGER PRIMARY KEY,
            category_id INTEGER,
            name TEXT
        );
        CREATE TABLE sde_types (
            type_id INTEGER PRIMARY KEY,
            group_id INTEGER,
            category_id INTEGER,
            name TEXT,
            faction_id INTEGER,
            volume REAL
        );
        CREATE TABLE sde_dungeons (
            dungeon_id INTEGER PRIMARY KEY,
            name TEXT,
            archetype_id INTEGER,
            faction_id INTEGER,
            difficulty INTEGER
        );
        CREATE TABLE sde_missions (
            mission_id INTEGER PRIMARY KEY,
            name TEXT,
            is_courier INTEGER,
            lp_alpha INTEGER,
            lp_omega INTEGER,
            has_standing_rewards INTEGER
        );
        """
    )

    groups = [
        (550, 11, "Asteroid Angel Cartel Frigate"),
        (600, 11, "Deadspace Guristas Battlecruiser"),
        (601, 11, "Deadspace Angel Cartel Frigate"),
        (602, 11, "Deadspace Angel Cartel Cruiser"),
        (603, 11, "Deadspace Serpentis Battleship"),
        (630, 11, "Deadspace Serpentis Battlecruiser"),
        (319, 11, "Large Collidable Structure"),
        (366, 2, "Acceleration Gate"),
        (25, 6, "Frigate"),
        (100, 18, "Combat Drone"),
        (101, 87, "Light Fighter"),
        (459, 25, "Harvestable Ore"),
        (314, 17, "Commodities"),
        (671, 11, "Mission Caldari State Frigate"),
        (20001, 11, "Destructible Sentry Gun"),
        (20002, 23, "Stasis Webification Battery"),
        (20003, 11, "Deadspace Angel Cartel Battlecruiser"),
        (20004, 11, "Deadspace Angel Cartel Battleship"),
    ]
    conn.executemany("INSERT INTO sde_groups VALUES (?, ?, ?)", groups)

    types = [
        # Category 6/18/87 player entities (must NEVER be matched for NPCs)
        (6001, 25, 6, "SynthAlpha", None, 100.0),
        (6002, 25, 6, "SynthBeta", None, 100.0),
        (6003, 100, 18, "SynthBeta I", None, 5.0),
        (6004, 101, 87, "SynthGamma", None, 10.0),
        # Ships / NPCs (Category 11 Entity)
        (10001, 600, 11, "Synth Pithatis Enforcer", None, 100.0),
        (10002, 600, 11, "Synth Pithatis Executor", None, 100.0),
        (10003, 630, 11, "Synth Corelatis Squad Leader", None, 100.0),
        (10004, 630, 11, "Synth Corelatis Wing Leader", None, 100.0),
        (10005, 550, 11, "Synth Gistii Ambusher", None, 100.0),
        (10006, 550, 11, "Synth Gistii Raider", None, 100.0),
        (10007, 550, 11, "Unit-1 Swarm", None, 100.0),
        (10008, 550, 11, "Unit-2 Swarm", None, 100.0),
        (10009, 550, 11, "Synth Renamed Interceptor", None, 100.0),
        (10010, 671, 11, "Caldari Navy Frigate", 500001, 100.0),
        (10011, 550, 11, "SynthOrder SynthAlpha", None, 100.0),
        (10012, 550, 11, "SynthOrder SynthBeta", None, 100.0),
        (10013, 550, 11, "Render Alvi", None, 50.0),
        (10014, 601, 11, "Gistii Nomad", None, 50.0),
        (10015, 602, 11, "Gistum Nomad", None, 100.0),
        (10016, 603, 11, "Core High Admiral", None, 1000.0),
        (10017, 603, 11, "Core Lord Admiral", None, 1000.0),
        (10018, 603, 11, "Core Admiral", None, 1000.0),
        (10019, 601, 11, "Gistii OnlyFrig", None, 50.0),
        (10020, 20003, 11, "Gistatis OnlyBC", None, 100.0),
        (10021, 602, 11, "Gistum OnlyCruiser", None, 100.0),
        (10022, 319, 11, "Blood Raider Bunker", None, 1000.0),
        # Structures & Gates (including Drone Bunker & Cat 23 Stasis Webification Battery + multi-tier sentries)
        (17831, 366, 2, "Acceleration Gate", None, 1000.0),
        (16757, 319, 11, "Angel Bunker", None, 1000.0),
        (16796, 319, 11, "Guristas Bunker", None, 1000.0),
        (18012, 319, 11, "Drone Bunker", None, 1000.0),
        (18013, 319, 11, "Outpost", None, 1000.0),
        (17605, 20001, 11, "Angel Stasis Tower", None, 1000.0),
        (17607, 20001, 11, "Guristas Stasis Tower", None, 1000.0),
        (17178, 20002, 23, "Stasis Webification Battery", None, 1000.0),
        (17147, 20001, 11, "Tower Sentry Angel I", None, 1000.0),
        (17146, 20001, 11, "Tower Sentry Angel II", None, 1000.0),
        (16740, 20001, 11, "Tower Sentry Angel III", None, 1000.0),
        (28139, 20001, 11, "Angel Energy Neutralizer Sentry I", None, 1000.0),
        (28144, 20001, 11, "Angel Energy Neutralizer Sentry II", None, 1000.0),
        (28150, 20001, 11, "Angel Energy Neutralizer Sentry III", None, 1000.0),
        # Ore & Courier Commodity (both bare item and crate item)
        (28618, 459, 25, "Synthium Ore", None, 0.3),
        (29570, 314, 17, "Crate of Synth Supplies", None, 20.0),
        (29571, 314, 17, "Synth Supplies", None, 5.0),
    ]
    conn.executemany("INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?)", types)

    dungeons = [
        (4001, "Synthetic Raid", 4, 500010, 3),
        (4002, "Synthetic Raid", 4, 500011, 4),
        (4003, "Synthetic Multi-Part (1 of 3)", 1, 500011, 2),
        (4004, "Different Dungeon Name", 2, 500010, 3),
    ]
    conn.executemany("INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)", dungeons)

    missions = [
        (9001, "Synthetic Multi-Part (1 of 3)", 0, 100, 150, 1),
        (9002, "Synthetic Supply Run", 1, 50, 75, 1),
        (9003, "Synthetic Ore Extraction", 0, 80, 120, 1),
        (9004, "Synthetic Linked Mission", 0, 120, 180, 1),
    ]
    conn.executemany("INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?)", missions)

    conn.commit()
    conn.close()

    mission_ext_path.write_text(
        json.dumps({"mission_dungeons": {"9001": 4003, "9004": 4004}, "rnd_agent_skills": {}}),
        encoding="utf-8",
    )


class TestWikiPveImporter(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.sde_db = self.root / "sde.sqlite"
        self.mission_ext = self.root / "mission_ext.json"
        _build_synthetic_sde(self.sde_db, self.mission_ext)
        self.sde = imp.SdeIndex(self.sde_db, self.mission_ext)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_short_form_name_expansion_variants(self) -> None:
        # Prefix sharing
        names, ell = self.sde.expand_short_names("Synth Pithatis Enforcer/Executor")
        self.assertFalse(ell)
        self.assertEqual(names, ["Synth Pithatis Enforcer", "Synth Pithatis Executor"])

        # Circumfix sharing (prefix + suffix)
        names2, _ = self.sde.expand_short_names("Synth Corelatis Squad/Wing Leader")
        self.assertEqual(names2, ["Synth Corelatis Squad Leader", "Synth Corelatis Wing Leader"])

        # Parenthetical alternation
        names3, _ = self.sde.expand_short_names("Synth Gistii (Ambusher/Raider)")
        self.assertEqual(names3, ["Synth Gistii Ambusher", "Synth Gistii Raider"])

        # Hyphen alternation
        names4, _ = self.sde.expand_short_names("Unit-1/2 Swarm")
        self.assertEqual(names4, ["Unit-1 Swarm", "Unit-2 Swarm"])

        # Ellipsis indicator preserves flag and expands explicit items
        names5, ell5 = self.sde.expand_short_names("Synth Gistii Ambusher/...")
        self.assertTrue(ell5)
        self.assertEqual(names5, ["Synth Gistii Ambusher"])

    def test_count_range_parsing(self) -> None:
        self.assertEqual(imp.parse_count("5-6"), {"min": 5, "max": 6, "raw": "5-6"})
        self.assertEqual(imp.parse_count("3 – 4"), {"min": 3, "max": 4, "raw": "3 - 4"})
        self.assertEqual(imp.parse_count("7"), {"min": 7, "max": 7, "raw": "7"})
        self.assertEqual(imp.parse_count("?"), {"min": None, "max": None, "raw": "?"})
        self.assertEqual(imp.parse_count("2+"), {"min": 2, "max": None, "raw": "2+"})

    def test_trigger_classification_all_kinds_and_unclassified(self) -> None:
        cases = [
            ("Triggers next wave", "this_ship_destroyed", "explicit"),
            ("Last ship killed in group", "last_in_group_destroyed", "explicit"),
            ("Last of this class destroyed", "last_of_class_destroyed", "explicit"),
            ("Spawns when approached within 30km", "ship_approached", "explicit"),
            ("Arrives after 2 minutes", "timed", "explicit"),
            ("Destroying completes the mission objective", "objective_complete", "explicit"),
            ("Mysterious cryptic whistle signal", None, "unclassified"),
        ]
        for text, expected_kind, expected_state in cases:
            parsed = imp.classify_trigger(text)
            self.assertIsNotNone(parsed)
            assert parsed is not None
            self.assertEqual(parsed["kind"], expected_kind, text)
            self.assertEqual(parsed["classification_state"], expected_state, text)
            self.assertIsNone(parsed["inference_rule"])
            self.assertEqual(parsed["original_text"], text)
            if expected_state == "unclassified":
                self.assertIsNone(parsed["condition"])

    def test_ewar_point_drop_and_note_alias(self) -> None:
        pt_scram = imp.parse_point("scram")
        self.assertEqual(pt_scram, "warp_scramble")
        ew = imp.parse_ewar(
            [
                "{{icon|web|24|Stasis webifier}}",
                "ECM",
                "[[File:Icon remote armor repair i.png|24px]]",
            ],
            pt_scram,
        )
        self.assertEqual(ew["tags"], ["ecm", "rr", "warp_scramble", "web"])

        res = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Old Legacy Frigate",
            note="Renamed to Synth Renamed Interceptor",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res["status"], "resolved_exact")
        self.assertEqual(res["type_ids"], [10009])

    def test_generic_structure_resolved_within_page_faction(self) -> None:
        # Faction-qualified "{fac} Bunker" must beat "Drone Bunker" on non-Drone pages
        res_angel = self.sde.resolve_npc_row(
            npc_class="Structure",
            raw_name="Bunker",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_angel["status"], "resolved_structure_faction")
        self.assertEqual(res_angel["type_ids"], [16757])

        res_drone_bunker = self.sde.resolve_npc_row(
            npc_class="Structure",
            raw_name="Bunker",
            note="",
            page_factions=["Rogue Drones"],
        )
        self.assertEqual(res_drone_bunker["status"], "resolved_structure_faction")
        self.assertEqual(res_drone_bunker["type_ids"], [18012])

        res_guristas = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Stasis Tower",
            note="",
            page_factions=["Guristas Pirates"],
        )
        self.assertEqual(res_guristas["status"], "resolved_structure_faction")
        self.assertEqual(res_guristas["type_ids"], [17607])

        # Starbase Stasis Tower on Angel page must resolve to Cat 11 Angel Stasis Tower,
        # NOT Cat 23 POS Stasis Webification Battery (while falling back when no faction tower exists)
        res_starbase = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Starbase Stasis Tower",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_starbase["status"], "resolved_structure_faction")
        self.assertEqual(res_starbase["type_ids"], [17605])

        res_fallback_pos = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Starbase Stasis Tower",
            note="",
            page_factions=["Mordu's Legion Command"],
        )
        self.assertEqual(res_fallback_pos["status"], "resolved_structure_faction")
        self.assertEqual(res_fallback_pos["type_ids"], [17178])

    def test_multi_room_bracketed_title_and_sde_matching(self) -> None:
        wiki_text = (
            "{{Missiondetails|type=Encounter|level=3|faction1=Guristas Pirates|"
            "objective=Eliminate synthetic pirates.}}\n"
            "=== Pocket 1 ===\n"
            "{{NPCTableHead|Initial Defenders}}\n"
            "{{NPCTableRow|Battlecruiser|5-6|Synth Pithatis Enforcer/Executor|ewar=Web|ewar2=Neut|point=scram|trigger=Last of class destroyed}}\n"
            "{{NPCTableRow|Acceleration Gate|1|Gate to Inner Sanctum}}\n"
            "=== Pocket 2 ===\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Structure|1|Bunker|drop=Synth Logbook|cargo=20m3|trigger=Bizarre custom phrasing}}\n"
            "{{NPCTableSeparator|Group 2}}\n"
            "{{NPCTableRow|Frigate|1|Unknown Ghost Ship}}\n"
        )
        parsed = imp.parse_wiki_page(
            title="Synthetic Raid (Guristas Pirates) (Level 3)",
            categories=["Security missions"],
            wikitext=wiki_text,
            sde=self.sde,
        )
        assert parsed is not None
        self.assertEqual(parsed["base_title"], "Synthetic Raid")
        self.assertEqual(parsed["title_qualifiers"], ["Guristas Pirates", "Level 3"])
        unit = parsed["units"][0]
        self.assertEqual(unit["levels"], [3])
        self.assertEqual(unit["factions"], ["Guristas Pirates"])
        # Disambiguates dungeon 4001 (Guristas diff 3) vs 4002 (Angel diff 4)!
        self.assertEqual(unit["sde_matches"]["dungeon_ids"], [4001])

        self.assertEqual(len(unit["rooms"]), 2)
        r1, r2 = unit["rooms"]
        self.assertEqual(r1["room_name"], "Pocket 1")
        self.assertTrue(r1["has_acceleration_gate"])
        self.assertFalse(r2["has_acceleration_gate"])
        self.assertEqual(len(r2["waves"]), 2)

        first_spawn = r1["waves"][0]["spawns"][0]
        self.assertEqual(first_spawn["count"], {"min": 5, "max": 6, "raw": "5-6"})
        self.assertEqual(first_spawn["resolution"]["type_ids"], [10001, 10002])
        self.assertEqual(first_spawn["ewar"]["tags"], ["neut", "warp_scramble", "web"])
        self.assertEqual(first_spawn["trigger"]["kind"], "last_of_class_destroyed")

        bunker_spawn = r2["waves"][0]["spawns"][0]
        self.assertEqual(bunker_spawn["resolution"]["type_ids"], [16796])
        self.assertIsNone(bunker_spawn["trigger"]["kind"])
        self.assertEqual(bunker_spawn["trigger"]["classification_state"], "unclassified")
        self.assertIsNone(bunker_spawn["trigger"]["inference_rule"])
        self.assertEqual(bunker_spawn["trigger"]["original_text"], "Bizarre custom phrasing")

    def test_part_number_preserved_and_courier_mining_extracted(self) -> None:
        courier_text = (
            "{{Missiondetails|type=Courier|level=2|faction1=Amarr Empire|"
            "objective=Deliver 14x Crate of Synth Supplies (280{{m3}}) to destination.}}\n"
        )
        mining_text = (
            "{{Missiondetails|type=Mining|level=2|faction1=ORE|"
            "objective=Mine 4,500 units (1,350{{m3}}) of Synthium Ore.}}\n"
        )
        part_text = (
            "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Clear camp.}}\n"
            "{{NPCTableHead|Initial}}\n"
            "{{NPCTableRow|Structure|1|Stasis Tower}}\n"
        )

        p_courier = imp.parse_wiki_page("Synthetic Supply Run", ["Distribution Missions"], courier_text, self.sde)
        assert p_courier is not None
        c_pay = p_courier["units"][0]["courier"]
        assert c_pay is not None
        self.assertEqual(c_pay["item"], "Crate of Synth Supplies")
        self.assertEqual(c_pay["quantity"], 14)
        self.assertEqual(c_pay["volume_m3"], 280.0)
        self.assertEqual(c_pay["item_type_ids"], [29570])
        self.assertEqual(p_courier["units"][0]["sde_matches"]["mission_ids"], [9002])

        p_mining = imp.parse_wiki_page("Synthetic Ore Extraction", ["Mining Missions"], mining_text, self.sde)
        assert p_mining is not None
        m_pay = p_mining["units"][0]["mining"]
        assert m_pay is not None
        self.assertEqual(m_pay["ore_or_material"], "Synthium Ore")
        self.assertEqual(m_pay["quantity"], 4500)
        self.assertEqual(m_pay["volume_m3"], 1350.0)
        self.assertEqual(m_pay["ore_type_ids"], [28618])

        p_part = imp.parse_wiki_page("Synthetic Multi-Part (1 of 3) (Angel Cartel)", ["COSMOS missions"], part_text, self.sde)
        assert p_part is not None
        self.assertEqual(p_part["base_title"], "Synthetic Multi-Part (1 of 3)")
        self.assertEqual(p_part["units"][0]["sde_matches"]["mission_ids"], [9001])
        self.assertEqual(p_part["units"][0]["sde_matches"]["dungeon_ids"], [4003])

    def test_deterministic_byte_identical_runs_and_report_accounting(self) -> None:
        wiki_dir = self.root / "wiki"
        wt_dir = wiki_dir / "wikitext"
        wt_dir.mkdir(parents=True)

        articles_meta = {}
        pages = {
            "Synthetic_Raid": (
                "Synthetic Raid (Angel Cartel) (Level 4)",
                ["Security missions"],
                (
                    "{{Missiondetails|type=Encounter|level=4|faction1=Angel Cartel|objective=Clear site.}}\n"
                    "{{NPCTableHead|Initial}}\n"
                    "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher/NoSuchShip|trigger=Mystery phrase}}\n"
                    "{{NPCTableRow|Frigate|1|Totally Missing Name}}\n"
                ),
            ),
            "Synthetic_Ore": (
                "Synthetic Ore Extraction",
                ["Mining Missions"],
                "{{Missiondetails|type=Mining|level=1|faction1=ORE|objective=Mine 100 units (30{{m3}}) of Synthium Ore.}}\n",
            ),
        }
        for slug, (title, cats, body) in pages.items():
            (wt_dir / f"{slug}.wiki").write_text(body, encoding="utf-8")
            articles_meta[slug] = {
                "title": title,
                "wikitext_path": f"wikitext/{slug}.wiki",
                "categories": cats,
            }
        (wiki_dir / "catalog.json").write_text(
            json.dumps({"summary": {"article_count": len(pages)}, "articles": articles_meta}),
            encoding="utf-8",
        )

        out1_c = self.root / "out1_content.json"
        out1_r = self.root / "out1_report.json"
        out2_c = self.root / "out2_content.json"
        out2_r = self.root / "out2_report.json"

        imp.run_import(wiki_dir, self.sde_db, self.mission_ext, out1_c, out1_r)
        imp.run_import(wiki_dir, self.sde_db, self.mission_ext, out2_c, out2_r)

        self.assertEqual(
            hashlib.sha256(out1_c.read_bytes()).hexdigest(),
            hashlib.sha256(out2_c.read_bytes()).hexdigest(),
        )
        self.assertEqual(
            hashlib.sha256(out1_r.read_bytes()).hexdigest(),
            hashlib.sha256(out2_r.read_bytes()).hexdigest(),
        )

        content = json.loads(out1_c.read_text(encoding="utf-8"))
        self.assertEqual(content["header"]["schema_version"], 2)
        report = json.loads(out1_r.read_text(encoding="utf-8"))
        totals = report["totals"]
        self.assertEqual(totals["npc_rows_total"], 2)
        self.assertEqual(totals["npc_rows_partly_resolved"], 1)
        self.assertEqual(totals["npc_rows_unresolved"], 1)
        self.assertEqual(totals["trigger_unclassified_count"], 1)
        self.assertEqual(len(report["partly_resolved_rows"]), 1)
        self.assertEqual(report["partly_resolved_rows"][0]["unresolved_parts"], ["Synth Gistii NoSuchShip"])
        self.assertEqual(len(report["unresolved_rows"]), 1)
        self.assertEqual(report["unresolved_rows"][0]["reason"], "unmatched_name_in_sde")
        self.assertEqual(len(report["unclassified_triggers"]), 1)
        self.assertEqual(report["unclassified_triggers"][0]["original_text"], "Mystery phrase")

    def test_class_candidate_guard_and_multi_unit_page(self) -> None:
        trig = imp.classify_trigger("Triggers next spawn")
        assert trig is not None
        self.assertEqual(trig["kind"], "this_ship_destroyed")

        # Named custom spawn with EVE faction/role maps to faction+class candidate IDs
        res_outlaw = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Caldari Outlaw",
            note="",
            page_factions=["Caldari State"],
        )
        self.assertEqual(res_outlaw["status"], "resolved_class_candidates")
        self.assertEqual(res_outlaw["candidate_type_ids"], [10010])

        # Arbitrary non-EVE string must stay unresolved rather than guessing class candidates
        res_bogus = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Totally Missing Name",
            note="",
            page_factions=["Caldari State"],
        )
        self.assertEqual(res_bogus["status"], "unresolved")
        self.assertEqual(res_bogus["reason"], "unmatched_name_in_sde")

        # Multi-unit page splits cleanly by Missiondetails sections
        multi_wiki = (
            "== Level 1 ==\n"
            "{{Missiondetails|type=Encounter|level=1|faction1=Angel Cartel|objective=Clear outpost.}}\n"
            "{{NPCTableHead|Initial}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n"
            "== Level 2 ==\n"
            "{{Missiondetails|type=Encounter|level=2|faction1=Guristas Pirates|objective=Clear depot.}}\n"
            "{{NPCTableHead|Wave 1}}\n"
            "{{NPCTableRow|Battlecruiser|1|Synth Pithatis Enforcer}}\n"
        )
        parsed = imp.parse_wiki_page("Synthetic Ladder", ["Security missions"], multi_wiki, self.sde)
        assert parsed is not None
        self.assertEqual(len(parsed["units"]), 2)
        u1, u2 = parsed["units"]
        self.assertEqual(u1["entry_id"], "Synthetic Ladder#1")
        self.assertEqual(u1["levels"], [1])
        self.assertEqual(u1["factions"], ["Angel Cartel"])
        self.assertEqual(u2["entry_id"], "Synthetic Ladder#2")
        self.assertEqual(u2["levels"], [2])
        self.assertEqual(u2["factions"], ["Guristas Pirates"])

    def test_cli_verify_passes_and_enforces_threshold(self) -> None:
        wiki_dir = self.root / "verify_wiki"
        wt_dir = wiki_dir / "wikitext"
        wt_dir.mkdir(parents=True)
        (wt_dir / "Clean_Site.wiki").write_text(
            (
                "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n"
                "{{NPCTableHead|Initial}}\n"
                "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n"
                "{{NPCTableRow|Structure|1|Stasis Tower}}\n"
            ),
            encoding="utf-8",
        )
        (wiki_dir / "catalog.json").write_text(
            json.dumps(
                {
                    "summary": {"article_count": 1},
                    "articles": {
                        "Clean_Site": {
                            "title": "Synthetic Raid",
                            "wikitext_path": "wikitext/Clean_Site.wiki",
                            "categories": ["Combat Anomalies"],
                        }
                    },
                }
            ),
            encoding="utf-8",
        )

        out_c = self.root / "verify_content.json"
        out_r = self.root / "verify_report.json"
        out_buf = io.StringIO()
        err_buf = io.StringIO()
        with contextlib.redirect_stdout(out_buf), contextlib.redirect_stderr(err_buf):
            rc_ok = imp.main(
                [
                    "--wiki-dir",
                    str(wiki_dir),
                    "--sde-db",
                    str(self.sde_db),
                    "--mission-ext",
                    str(self.mission_ext),
                    "--out-content",
                    str(out_c),
                    "--out-report",
                    str(out_r),
                    "--verify",
                ]
            )
        self.assertEqual(rc_ok, 0)
        self.assertIn("VERIFY PASS:", out_buf.getvalue())
        self.assertEqual(err_buf.getvalue(), "")
        self.assertTrue(out_c.is_file())
        self.assertTrue(out_r.is_file())

        # Unresolved rows below threshold must cause --verify to exit non-zero
        (wt_dir / "Clean_Site.wiki").write_text(
            (
                "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n"
                "{{NPCTableHead|Initial}}\n"
                "{{NPCTableRow|Frigate|1|Totally Missing Name}}\n"
            ),
            encoding="utf-8",
        )
        out_buf_fail = io.StringIO()
        err_buf_fail = io.StringIO()
        with contextlib.redirect_stdout(out_buf_fail), contextlib.redirect_stderr(err_buf_fail):
            rc_fail = imp.main(
                [
                    "--wiki-dir",
                    str(wiki_dir),
                    "--sde-db",
                    str(self.sde_db),
                    "--mission-ext",
                    str(self.mission_ext),
                    "--out-content",
                    str(out_c),
                    "--out-report",
                    str(out_r),
                    "--verify",
                ]
            )
        self.assertEqual(rc_fail, 1)
        self.assertIn("VERIFY FAIL:", err_buf_fail.getvalue())

    def test_category_exclusion_and_shared_prefix_priority(self) -> None:
        # Shared prefix must beat Cat 6/18/87 player hulls/drones/fighters
        res = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="SynthOrder SynthAlpha/SynthBeta",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res["status"], "resolved_exact")
        self.assertEqual(res["expanded_names"], ["SynthOrder SynthAlpha", "SynthOrder SynthBeta"])
        self.assertEqual(res["type_ids"], [10011, 10012])

        # Standalone Cat 6/18/87 names must never leak into NPC resolution
        for hull in ("SynthAlpha", "SynthBeta", "SynthGamma"):
            leak = self.sde.resolve_npc_row(
                npc_class="Frigate",
                raw_name=hull,
                note="",
                page_factions=["Angel Cartel"],
            )
            self.assertEqual(leak["type_ids"], [])
            self.assertNotEqual(leak["status"], "resolved_exact")

        # Parenthesized outer/inner alternation must expand cleanly without premature stripping
        res_outer = self.sde.resolve_npc_row(
            npc_class="Battlecruiser",
            raw_name="(Synth Pithatis Enforcer/Executor)",
            note="",
            page_factions=["Guristas Pirates"],
        )
        self.assertEqual(res_outer["expanded_names"], ["Synth Pithatis Enforcer", "Synth Pithatis Executor"])
        self.assertEqual(res_outer["type_ids"], [10001, 10002])

        res_inner = self.sde.resolve_npc_row(
            npc_class="Battleship",
            raw_name="Core (High/Lord) Admiral",
            note="",
            page_factions=["Serpentis"],
        )
        self.assertEqual(res_inner["expanded_names"], ["Core High Admiral", "Core Lord Admiral"])
        self.assertEqual(res_inner["type_ids"], [10016, 10017])

    def test_rank_class_constraint_rogue_drones_and_no_default_angel(self) -> None:
        # Gistum (Cruiser) must NOT cross-substitute into Gistii (Frigate) even when Gistii OnlyFrig exists
        res_mismatch = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Gistum OnlyFrig",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_mismatch["type_ids"], [])

        # Gistii Nomad (Frigate) and Gistum Nomad (Cruiser) resolve strictly within their classes
        res_nomad_frig = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Gistii Nomad",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_nomad_frig["type_ids"], [10014])
        res_nomad_cru = self.sde.resolve_npc_row(
            npc_class="Cruiser",
            raw_name="Gistum Nomad",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_nomad_cru["type_ids"], [10015])

        # Rogue Drone suffix typo swap works when ship_class="Frigate" explicitly constrains hull class,
        # but unknown-class ("Drone") must NOT cross hull classes from Alvatis (BC) to Alvi (Frigate)
        res_drone = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Render Alvatis",
            note="",
            page_factions=["Rogue Drones"],
        )
        self.assertEqual(res_drone["status"], "resolved_exact")
        self.assertEqual(res_drone["type_ids"], [10013])

        res_drone_unknown_cls = self.sde.resolve_npc_row(
            npc_class="Drone",
            raw_name="Render Alvatis",
            note="",
            page_factions=["Rogue Drones"],
        )
        self.assertEqual(res_drone_unknown_cls["type_ids"], [])

        # Empty page_factions must NEVER synthesize Drone Bunker or Angel Cartel fallback
        res_nofac = self.sde.resolve_npc_row(
            npc_class="Structure",
            raw_name="Bunker",
            note="",
            page_factions=[],
        )
        self.assertEqual(res_nofac["status"], "unresolved")
        self.assertEqual(res_nofac["type_ids"], [])

    def test_candidate_buckets_not_counted_by_verify_gate(self) -> None:
        wiki_dir = self.root / "wiki_cand"
        wt_dir = wiki_dir / "wikitext"
        wt_dir.mkdir(parents=True)
        (wt_dir / "Cand_Site.wiki").write_text(
            (
                "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n"
                "{{NPCTableHead|Initial}}\n"
                "{{NPCTableRow|Frigate|2|Angel Security Guard}}\n"
                "{{NPCTableRow|Cruiser|1|Gistum}}\n"
            ),
            encoding="utf-8",
        )
        (wiki_dir / "catalog.json").write_text(
            json.dumps(
                {
                    "summary": {"article_count": 1},
                    "articles": {
                        "Cand_Site": {
                            "title": "Synthetic Raid",
                            "wikitext_path": "wikitext/Cand_Site.wiki",
                            "categories": ["Combat Anomalies"],
                        }
                    },
                }
            ),
            encoding="utf-8",
        )
        out_c = self.root / "cand_content.json"
        out_r = self.root / "cand_report.json"
        out_buf = io.StringIO()
        err_buf = io.StringIO()
        with contextlib.redirect_stdout(out_buf), contextlib.redirect_stderr(err_buf):
            rc = imp.main(
                [
                    "--wiki-dir",
                    str(wiki_dir),
                    "--sde-db",
                    str(self.sde_db),
                    "--mission-ext",
                    str(self.mission_ext),
                    "--out-content",
                    str(out_c),
                    "--out-report",
                    str(out_r),
                    "--verify",
                ]
            )
        self.assertEqual(rc, 1)
        self.assertIn("VERIFY FAIL:", err_buf.getvalue())
        self.assertIn("< 85.0%", err_buf.getvalue())
        rep = json.loads(out_r.read_text(encoding="utf-8"))
        self.assertEqual(rep["totals"]["npc_rows_resolved_total"], 0)
        self.assertEqual(rep["totals"]["npc_rows_resolved_percent"], 0.0)
        self.assertEqual(rep["totals"]["npc_rows_candidate_mapped_total"], 1)
        self.assertEqual(rep["totals"]["npc_rows_candidate_mapped_percent"], 50.0)
        self.assertEqual(len(rep["unresolved_rows"]), 2)
        reasons = {r["raw_name"]: r["reason"] for r in rep["unresolved_rows"]}
        self.assertEqual(reasons["Angel Security Guard"], "custom_spawn_name_mapped_via_faction_class")
        self.assertEqual(reasons["Gistum"], "bare_family_prefix_only")

    def test_variant_splitting_ghost_wave_pruning_triggers_and_crates(self) -> None:
        wiki_text = (
            "{{CMBSiteInfo|name=Synthetic Raid|faction=Serpentis|rating=3}}\n"
            "== Type 1 - Alpha Variant ==\n"
            "{{NPCTableHead|Initial Hostiles}}\n"
            "{{NPCTableSeparator|Group 1 (15km)}}\n"
            "{{NPCTableRow|Battleship|1|Core High Admiral|trigger=Destruction of the last Core Admiral triggers the next wave}}\n"
            "{{NPCTableSeparator|Group 2 (30km)}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher|trigger=Approach within 500m}}\n"
            "{{NPCTableHead|Wave 1}}\n"
            "{{NPCTableRow|Battleship|1|Core Lord Admiral|trigger=Last Synth Conquistador triggers next wave}}\n"
            "== Type 2 - Beta Variant ==\n"
            "{{NPCTableHead|Initial Defenders}}\n"
            "{{NPCTableRow|Frigate|3|Synth Gistii Raider}}\n"
        )
        parsed = imp.parse_wiki_page(
            title="Synthetic Haven",
            categories=["Combat Anomalies"],
            wikitext=wiki_text,
            sde=self.sde,
        )
        assert parsed is not None
        self.assertEqual(len(parsed["units"]), 2)
        u1, u2 = parsed["units"]
        self.assertEqual(u1["variant_name"], "Type 1 - Alpha Variant")
        self.assertEqual(u2["variant_name"], "Type 2 - Beta Variant")
        self.assertEqual(u1["variant_index"], 1)
        self.assertEqual(u1["variant_count"], 2)

        waves1 = u1["rooms"][0]["waves"]
        # Empty ghost wave from Initial Hostiles before Group 1 must be pruned!
        self.assertTrue(all(len(w["spawns"]) > 0 for w in waves1))
        self.assertEqual(len(waves1), 3)
        self.assertEqual(waves1[0]["timing"], "simultaneous_initial_group")
        self.assertEqual(waves1[1]["timing"], "simultaneous_initial_group")
        self.assertEqual(waves1[2]["timing"], "sequential_wave")

        self.assertEqual(waves1[0]["spawns"][0]["trigger"]["kind"], "last_of_class_destroyed")
        self.assertEqual(waves1[1]["spawns"][0]["trigger"]["kind"], "ship_approached")
        self.assertEqual(waves1[2]["spawns"][0]["trigger"]["kind"], "last_of_class_destroyed")
        self.assertEqual(imp.classify_trigger("Approach within 500m")["kind"], "ship_approached")
        arrives_5m = imp.classify_trigger("Arrives after 5m")
        assert arrives_5m is not None
        self.assertIsNone(arrives_5m["kind"])
        self.assertEqual(arrives_5m["classification_state"], "unclassified")
        self.assertEqual(arrives_5m["original_text"], "Arrives after 5m")

        # Plural "large crates of " lookup
        self.assertEqual(self.sde.lookup_item_type_ids("Large Crates of Synth Supplies"), [29570])

    def test_dual_verify_thresholds_enforced(self) -> None:
        wiki_dir = self.root / "wiki_dual_gate"
        wt_dir = wiki_dir / "wikitext"
        wt_dir.mkdir(parents=True)
        (wiki_dir / "catalog.json").write_text(
            json.dumps(
                {
                    "summary": {"article_count": 1},
                    "articles": {
                        "Dual_Site": {
                            "title": "Synthetic Raid",
                            "wikitext_path": "wikitext/Dual_Site.wiki",
                            "categories": ["Combat Anomalies"],
                        }
                    },
                }
            ),
            encoding="utf-8",
        )
        out_c = self.root / "dual_content.json"
        out_r = self.root / "dual_report.json"

        def run_case(rows: list[str]) -> tuple[int, str, str]:
            body = "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n{{NPCTableHead|Initial}}\n"
            body += "".join(f"{{{{NPCTableRow|Frigate|1|{name}}}}}\n" for name in rows)
            (wt_dir / "Dual_Site.wiki").write_text(body, encoding="utf-8")
            out_buf = io.StringIO()
            err_buf = io.StringIO()
            with contextlib.redirect_stdout(out_buf), contextlib.redirect_stderr(err_buf):
                rc = imp.main(
                    [
                        "--wiki-dir",
                        str(wiki_dir),
                        "--sde-db",
                        str(self.sde_db),
                        "--mission-ext",
                        str(self.mission_ext),
                        "--out-content",
                        str(out_c),
                        "--out-report",
                        str(out_r),
                        "--verify",
                    ]
                )
            return rc, out_buf.getvalue(), err_buf.getvalue()

        # 9 concrete + 1 candidate-only = 90.0% concrete (>= 85.0%), 100.0% candidate_mapped (>= 90.0%) -> PASS
        rc_pass, out_pass, err_pass = run_case(["SynthOrder SynthAlpha"] * 9 + ["Angel Security Guard"])
        self.assertEqual(rc_pass, 0)
        self.assertEqual(err_pass, "")
        self.assertIn("concrete_resolved=9/10 (90.0% >= 85.0%)", out_pass)
        self.assertIn("candidate_mapped=10/10 (100.0% >= 90.0%)", out_pass)
        self.assertRegex(out_pass, r"content_sha256=[0-9a-f]{64} report_sha256=[0-9a-f]{64}")

        # 8 concrete + 1 candidate-only + 1 unmatched = 80.0% concrete (< 85.0%), 90.0% candidate_mapped -> FAIL concrete
        rc_fail_conc, _, err_fail_conc = run_case(
            ["SynthOrder SynthAlpha"] * 8 + ["Angel Security Guard", "Totally Missing Name"]
        )
        self.assertEqual(rc_fail_conc, 1)
        self.assertIn("VERIFY FAIL: NPC concrete resolved percent 80.0% < 85.0%", err_fail_conc)
        self.assertNotIn("candidate-mapped percent", err_fail_conc)

        # 17 concrete + 1 bare family prefix (resolved_family_candidates) + 2 unmatched = 85.0% concrete (>= 85.0%),
        # 85.0% candidate_mapped (< 90.0%) -> FAIL candidate (family prefix must NOT count as faction+class candidate)
        rc_fail_cand, _, err_fail_cand = run_case(
            ["SynthOrder SynthAlpha"] * 17 + ["Gistum"] + ["Totally Missing Name"] * 2
        )
        self.assertEqual(rc_fail_cand, 1)
        self.assertIn("VERIFY FAIL: NPC candidate-mapped percent 85.0% < 90.0%", err_fail_cand)
        self.assertNotIn("concrete resolved percent", err_fail_cand)

    def test_r2_p1_fixes_substring_sentries_crates_and_sqlite_ro(self) -> None:
        # 1. Battlecruisers must NEVER pollute Cruiser candidate pools
        cru_pool = self.sde.faction_class_groups.get(("Angel Cartel", "cruiser"), [])
        bc_pool = self.sde.faction_class_groups.get(("Angel Cartel", "battlecruiser"), [])
        self.assertIn(10015, cru_pool)
        self.assertIn(10021, cru_pool)
        self.assertNotIn(10020, cru_pool)
        self.assertIn(10020, bc_pool)

        # 2. Word-boundary & strict faction inference avoids core/blood false positives
        for neutral_text in (
            "Datacore",
            "Power Core",
            "Core Complexion",
            "Core Garrison",
            "The Score",
            "Minecore Facilities",
            "The Blood-Stained Stars",
            "Bloodline",
        ):
            self.assertIsNone(imp.infer_faction_from_text(neutral_text), neutral_text)
        self.assertEqual(imp.infer_faction_from_text("Rogue Drone Core"), "Rogue Drones")
        self.assertEqual(imp.infer_faction_from_text("Contested Amarr Bastion of Blood"), "Amarr Empire")
        self.assertEqual(imp.infer_faction_from_text("Core High Admiral"), "Serpentis")
        self.assertEqual(imp.infer_faction_from_text("Blood Disciple"), "Blood Raider Covenant")

        # 3. Trailing " I" removal lets untiered sentries return all matching tiers (I/II/III)
        res_neut = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Angel Energy Neutralizer Sentry",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_neut["type_ids"], [28139, 28144, 28150])

        res_tws = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Tower Sentry Angel",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_tws["type_ids"], [16740, 17146, 17147])

        # 4. Crate lookup prefers crate-qualified item over bare commodity when both exist
        self.assertEqual(self.sde.lookup_item_type_ids("Large Crates of Synth Supplies"), [29570])

        # 5. Missing or 0-byte SQLite DB raises FileNotFoundError and NEVER creates a 0-byte stub
        missing_db = self.root / "never_created.sqlite"
        with self.assertRaises(FileNotFoundError):
            imp.SdeIndex(missing_db, self.mission_ext)
        self.assertFalse(missing_db.exists())

        empty_db = self.root / "zero_byte.sqlite"
        empty_db.write_bytes(b"")
        with self.assertRaises(FileNotFoundError):
            imp.SdeIndex(empty_db, self.mission_ext)

    def test_r2_p2_fixes_difflib_comma_ellipsis_unclosed_and_linked_dungeon(self) -> None:
        # 1. Difflib typo guard rejects distinct semantic words like Hunter vs Bunker
        res_hunter = self.sde.resolve_npc_row(
            npc_class="Battlecruiser",
            raw_name="Blood Raider Hunter",
            note="",
            page_factions=["Blood Raider Covenant"],
        )
        self.assertNotIn(10022, res_hunter["type_ids"])

        # 2. Stray comma in courier/mining objective never triggers int(",") ValueError
        comma_courier = imp.extract_courier_payload("Deliver , to station", self.sde)
        self.assertTrue(comma_courier is None or comma_courier["quantity"] is None)

        # 3. Ellipsis truncation indicator preserved on concrete exact matches
        res_ell = self.sde.resolve_npc_row(
            npc_class="Frigate",
            raw_name="Synth Gistii Ambusher/...",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_ell["status"], "resolved_exact")
        self.assertEqual(res_ell["type_ids"], [10005])
        self.assertTrue(res_ell.get("had_ellipsis"))

        # 4. Unclosed "{{" does not discard subsequent balanced templates
        wiki_unclosed = (
            "{{UnclosedTemplate|broken=1\n"
            "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n"
            "{{NPCTableHead|Initial}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
        )
        parsed_unclosed = imp.parse_wiki_page(
            title="Synthetic Raid",
            categories=["Combat Anomalies"],
            wikitext=wiki_unclosed,
            sde=self.sde,
        )
        assert parsed_unclosed is not None
        self.assertEqual(len(parsed_unclosed["units"]), 1)
        self.assertEqual(parsed_unclosed["units"][0]["rooms"][0]["waves"][0]["spawns"][0]["resolution"]["type_ids"], [10005])

        # 5. Bounded variant lookahead stops at next unit header
        wiki_two_units = (
            "{{CMBSiteInfo|name=Synthetic Raid|faction=Angel Cartel|rating=3}}\n"
            "== Main Section ==\n"
            "{{NPCTableHead|Initial}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{CMBSiteInfo|name=Synthetic Second|faction=Angel Cartel|rating=3}}\n"
            "== Type 1 - Sub Variant ==\n"
            "{{NPCTableHead|Initial}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
        )
        parsed_two = imp.parse_wiki_page(
            title="Synthetic Page",
            categories=["Combat Anomalies"],
            wikitext=wiki_two_units,
            sde=self.sde,
        )
        assert parsed_two is not None
        self.assertEqual(len(parsed_two["units"]), 2)
        self.assertIsNone(parsed_two["units"][0]["variant_name"])
        self.assertEqual(parsed_two["units"][1]["variant_name"], "Type 1 - Sub Variant")

        # 6. Linked mission_ext dungeon populates real sde_dungeons faction_id/difficulty
        m_res = self.sde.match_sde_page(["Synthetic Linked Mission"], ["Angel Cartel"], [3])
        self.assertEqual(m_res["mission_ids"], [9004])
        self.assertEqual(m_res["dungeon_ids"], [4004])
        self.assertEqual(m_res["dungeons"][0]["faction_id"], 500010)
        self.assertEqual(m_res["dungeons"][0]["difficulty"], 3)

    def test_track_a_three_state_schema_no_default_guessing(self) -> None:
        wiki_text = (
            "{{Missiondetails|type=Encounter|level=3|faction1=Guristas Pirates|"
            "objective=Destroy all pirate vessels and retrieve the drone core.|"
            "rewards=1,200,000 ISK|extra=Beware webs|standingloss=Minimal|shipsuggestion=Cruiser}}\n"
            "=== Pocket 1 ===\n"
            "{{NPCTableHead|Initial Hostiles (15-25km)}}\n"
            "{{NPCTableSeparator|Group 1}}\n"
            "{{NPCTableRow|Battlecruiser|2|Synth Pithatis Enforcer|trigger=Cryptic non-matching note}}\n"
            "{{NPCTableHead|Structures}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n"
            "{{NPCTableHead|Wave 2}}\n"
            "{{NPCTableRow|Frigate|3|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Reinforcement 2 (20-30km) (Triggered by destroying Core Admiral)}}\n"
            "{{NPCTableRow|Battleship|1|Core Admiral}}\n"
            "{{NPCTableHead|Group 3 (Triggers Wave 2)}}\n"
            "{{NPCTableRow|Cruiser|2|Gistum Nomad}}\n"
        )
        parsed = imp.parse_wiki_page(
            title="Synthetic Raid (Guristas Pirates) (Level 3)",
            categories=["Security missions"],
            wikitext=wiki_text,
            sde=self.sde,
        )
        assert parsed is not None
        u = parsed["units"][0]

        # Track B G2 classifies listed explicit objective prose while preserving original_text; unrecognized prose stays unclassified
        self.assertEqual(u["objective_kind"], "retrieve_item")
        self.assertEqual(u["objective"]["classification_state"], "explicit")
        self.assertIsNone(u["objective"]["inference_rule"])
        self.assertEqual(
            u["objective"]["original_text"],
            "Destroy all pirate vessels and retrieve the drone core.",
        )

        waves = u["rooms"][0]["waves"]
        self.assertEqual(len(waves), 5)

        # Wave 0: Simultaneous group under initial head -> inferred from table structure
        self.assertEqual(waves[0]["timing"], "simultaneous_initial_group")
        self.assertEqual(waves[0]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(waves[0]["wave_link"]["inference_rule"], "table_separator_initial_group")
        self.assertIsNone(waves[0]["wave_trigger"])
        self.assertEqual(waves[0]["distance_m"], {"min_m": 15000, "max_m": 25000})
        sp_trig = waves[0]["spawns"][0]["trigger"]
        self.assertIsNone(sp_trig["kind"])
        self.assertEqual(sp_trig["classification_state"], "unclassified")
        self.assertEqual(sp_trig["original_text"], "Cryptic non-matching note")

        # Wave 1: Head said 'Structures' but contains combat Frigate -> flips away from static_structures + updates wave_link!
        self.assertEqual(waves[1]["timing"], "sequential_wave")
        self.assertEqual(waves[1]["wave_link"]["classification_state"], "unclassified")
        self.assertIsNone(waves[1]["wave_link"]["inference_rule"])

        # Wave 2: Plain 'Wave 2' without trigger -> sequential_wave + unclassified wave_link (never guessed!)
        self.assertEqual(waves[2]["timing"], "sequential_wave")
        self.assertIsNone(waves[2]["wave_trigger"])
        self.assertEqual(waves[2]["wave_link"]["classification_state"], "unclassified")
        self.assertEqual(waves[2]["wave_link"]["original_text"], "Wave 2")

        # Wave 3: Multi-paren wave header extracts trigger from second paren AND numeric distance from first paren!
        self.assertIsNotNone(waves[3]["wave_trigger"])
        self.assertEqual(waves[3]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(waves[3]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertEqual(waves[3]["wave_link"]["classification_state"], "explicit")
        self.assertEqual(waves[3]["distance_m"], {"min_m": 20000, "max_m": 30000})

        # Verb-less passive 'Triggered by <Name>' stays unclassified rather than guessing on_destroy vs on_attack
        ambig = imp.classify_trigger("Triggered by Core Admiral")
        assert ambig is not None
        self.assertIsNone(ambig["kind"])
        self.assertEqual(ambig["classification_state"], "unclassified")

        # Wave 4: Outgoing '(Triggers Wave 2)' paren must NOT masquerade as incoming wave_trigger
        self.assertIsNone(waves[4]["wave_trigger"])
        self.assertEqual(waves[4]["wave_link"]["classification_state"], "unclassified")

        # Courier & Mining deterministically derive inferred objectives from Missiondetails payload
        p_courier = imp.parse_wiki_page(
            "Synthetic Supply Run",
            ["Distribution Missions"],
            "{{Missiondetails|type=Courier|level=2|faction1=Amarr Empire|objective=Deliver 14x Crate of Synth Supplies (280{{m3}}) to destination.}}\n",
            self.sde,
        )
        assert p_courier is not None
        obj_c = p_courier["units"][0]["objective"]
        self.assertEqual(obj_c["kind"], "deliver_item")
        self.assertEqual(obj_c["classification_state"], "inferred")
        self.assertEqual(obj_c["inference_rule"], "missiondetails_courier_payload")
        self.assertEqual(obj_c["target_type_ids"], [29570])

    def test_g5_distance_parsing_durations_and_unparsed_flagging(self) -> None:
        # Listed unambiguous distance patterns -> numeric {min_m, max_m}
        good_cases = [
            ("Initial (50-80km)", None, {"min_m": 50000, "max_m": 80000}),
            ("1st Reinforcement Spawn (13- 17km)", None, {"min_m": 13000, "max_m": 17000}),
            ("Wave 2 - 40km", None, {"min_m": 40000, "max_m": 40000}),
            ("Wave 1 - 35km - 55km from gate", None, {"min_m": 35000, "max_m": 55000}),
            ("Wave #2 - Spatial Rift proximity spawn (~5000m)", None, {"min_m": 5000, "max_m": 5000}),
            ("Approach within 500m", None, {"min_m": 500, "max_m": 500}),
            ("Outer Beacon (1.5 AU)", None, {"min_m": 224396806050, "max_m": 224396806050}),
            ("Initial Hostiles - Group 2", "Group 2 (10-25km)", {"min_m": 10000, "max_m": 25000}),
        ]
        for wname, grp, expected in good_cases:
            info = imp.extract_wave_distance_info(wname, group_name=grp)
            self.assertEqual(info["distance_m"], expected, wname)
            self.assertFalse(info["distance_unparsed"], wname)

        # Durations & ship counts MUST NOT parse as distances and MUST NOT flag distance_unparsed
        non_distance_cases = [
            "Timed spawn (30s)",
            "Wave 2 (2m)",
            "Wave 3 (1h)",
            "Reinforcements (2-5m)",
            "Delayed wave (60m)",
            "Arrives after 5m",
            "Wave 2 (after 3-5 minutes)",
            "2nd Reinforcement (2-4 NPCs)",
            "Boss wave (1-5 ships)",
        ]
        for label in non_distance_cases:
            info = imp.extract_wave_distance_info(label)
            self.assertIsNone(info["distance_m"], label)
            self.assertFalse(info["distance_unparsed"], label)

        # Unlisted/ambiguous distance strings MUST stay distance_m=None + flagged distance_unparsed=True (no guessing!)
        unparsed_cases = [
            ("Group 2 (200+ km)", None, "200+ km"),
            ("Initial defenders (60+ km)", None, "60+ km"),
            ("Initial (20km+)", None, "20km+"),
            ("Hostile NPCs (>35km)", None, ">35km"),
            ("Guard fleet (<10 km, Auto aggro)", None, "<10 km"),
            ("Wave 2 (40/80km, On destroying Control Tower)", None, "40/80km"),
            ("Wave 2 (50km to 80km)", None, "50km to 80km"),
            ("Wave 2 (50km or 80km)", None, "50km or 80km"),
            ("Wave 2 (between 20km and 40km)", None, "between 20km and 40km"),
            ("Group 2 at Amarr Control Tower (45k from warp-in)", None, "45k from"),
            ("Initial - Group A (km)", None, "(km)"),
            ("2nd Reinforcement Spawn (40- 45)", None, "(40- 45)"),
            ("Wave 2 (50 to 80 km)", None, "50 to 80 km"),
            ("Wave 2 (50..80km)", None, "50..80km"),
            ("Wave 2 (50km..80km)", None, "50km..80km"),
            ("Wave 2 (50km ± 10km)", None, "50km ± 10km"),
            ("Reinforcements (over 500m)", None, "over 500m"),
            ("Wave 2 (500m to 800m)", None, "500m to 800m"),
            ("Wave 2 (500/800m)", None, "500/800m"),
            ("Initial Hostiles (15-25km) - Group 2 (200+ km)", "Group 2 (200+ km)", "200+ km"),
        ]
        for wname, grp, expected_raw in unparsed_cases:
            info = imp.extract_wave_distance_info(wname, group_name=grp)
            self.assertIsNone(info["distance_m"], wname)
            self.assertTrue(info["distance_unparsed"], wname)
            self.assertEqual(info["distance_raw"], expected_raw, wname)

    def test_g3_g4_g5_end_to_end_real_structure_pipeline(self) -> None:
        # Build dedicated multi-level / multi-stage / escalation SDE + synthetic EVE wiki corpus
        sde_db2 = self.root / "g345_sde.sqlite"
        ext_json2 = self.root / "g345_ext.json"
        conn = sqlite3.connect(str(sde_db2))
        conn.executescript(
            """
            CREATE TABLE sde_categories (category_id INTEGER PRIMARY KEY, name TEXT);
            CREATE TABLE sde_groups (group_id INTEGER PRIMARY KEY, category_id INTEGER, name TEXT);
            CREATE TABLE sde_types (
                type_id INTEGER PRIMARY KEY, group_id INTEGER, category_id INTEGER,
                name TEXT, faction_id INTEGER, volume REAL
            );
            CREATE TABLE sde_dungeons (
                dungeon_id INTEGER PRIMARY KEY, name TEXT, archetype_id INTEGER,
                faction_id INTEGER, difficulty INTEGER
            );
            CREATE TABLE sde_missions (
                mission_id INTEGER PRIMARY KEY, name TEXT, is_courier INTEGER,
                lp_alpha INTEGER, lp_omega INTEGER, has_standing_rewards INTEGER
            );
            """
        )
        conn.executemany("INSERT INTO sde_categories VALUES (?, ?)", [(11, "Entity"), (2, "Celestial")])
        conn.executemany("INSERT INTO sde_groups VALUES (?, ?, ?)", [(601, 11, "Deadspace Angel Cartel Frigate")])
        conn.executemany(
            "INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?)",
            [
                (10005, 601, 11, "Synth Gistii Ambusher", None, 100.0),
                (10006, 601, 11, "Synth Gistii Raider", None, 100.0),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)",
            [
                # Same-named L2 and L4 mission dungeons + 2 identical expedition stages + DED site + target site
                (5001, "Alpha Clash", 4, 500011, 2),
                (5002, "Alpha Clash", 4, 500011, 4),
                (5101, "Chain Trail", 1, 500011, 3),
                (5102, "Chain Trail", 1, 500011, 3),
                (5201, "Crimson Complex", 1, 500006, 5),
                (5301, "Final Target Hideout", 1, 500011, 4),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?)",
            [
                (9101, "Alpha Clash", 0, 10, 20, 1),
                (9102, "Alpha Clash", 0, 40, 80, 1),
                (9201, "Storyline Series (1 of 2)", 0, 10, 20, 1),
                (9202, "Storyline Series (2 of 2)", 0, 20, 40, 1),
            ],
        )
        conn.commit()
        conn.close()
        ext_json2.write_text(json.dumps({"mission_dungeons": {"9101": 5001, "9102": 5002}}), encoding="utf-8")

        # Verify SdeIndex.dungeons_by_id stays immutable after mission_ext tagging
        local_sde = imp.SdeIndex(sde_db2, ext_json2)
        m_clash = local_sde.match_sde_page(["Alpha Clash"], ["Angel Cartel"], [2])
        self.assertEqual(m_clash["mission_ids"], [9101])
        self.assertEqual(m_clash["dungeon_ids"], [5001])
        self.assertNotIn("linked_via_mission_ext", local_sde.dungeons_by_id[5001])

        wiki_dir = self.root / "wiki_g345"
        wt_dir = wiki_dir / "wikitext"
        wt_dir.mkdir(parents=True)
        pages = {
            "Alpha_Clash_L2": (
                "Alpha Clash (Angel Cartel) (Level 2)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Clear L2.|rewards=500 ISK|extra=Hint L2}}\n"
                "{{NPCTableHead|Initial (15-25km)}}\n"
                "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher|trigger=Last ship killed triggers next wave}}\n"
                "{{NPCTableHead|Wave 2 (200+ km)}}\n"
                "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n",
            ),
            "Alpha_Clash_L4": (
                "Alpha Clash (Angel Cartel) (Level 4)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=4|faction1=Angel Cartel|objective=Clear L4.}}\n"
                "{{NPCTableHead|Initial (30km)}}\n"
                "{{NPCTableRow|Frigate|4|Synth Gistii Ambusher}}\n",
            ),
            "Chain_Trail": (
                "Chain Trail",
                ["Expeditions"],
                "{{CMBSiteInfo|name=Chain Trail|faction=Angel Cartel|rating=3}}\n"
                "== First Location ==\n"
                "{{NPCTableHead|Initial Defenders}}\n"
                "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
                "== Second Location ==\n"
                "{{NPCTableHead|Wave 2 (Triggered by destroying Synth Gistii Raider)}}\n"
                "{{NPCTableRow|Frigate|2|Synth Gistii Raider}}\n"
                "== Escalation ==\n"
                "{{MessageBox|On warpin: Ignore non-mission banner}}\n"
                "{{MessageBox|[[Final Target Hideout]]}}\n"
                "This site escalates to [[Final Target Hideout#Walkthrough]] or [[Unmatched Phantom Site]].\n"
                "{{cmbsite|Final Target Hideout}}\n",
            ),
            "Crimson_Complex_DED": (
                "Crimson Complex (DED)",
                ["DED Complexes"],
                "{{CMBSiteInfo|name=Crimson Complex|rating=5/10}}\n"
                "{{NPCTableHead|Initial}}\n"
                "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n",
            ),
            "Storyline_1": (
                "Storyline Series (1 of 2) (Angel Cartel)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Part 1.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n",
            ),
            "Storyline_2": (
                "Storyline Series (2 of 2) (Angel Cartel)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Part 2.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|2|Synth Gistii Raider}}\n",
            ),
            "Final_Target_Hideout": (
                "Final Target Hideout",
                ["Unrated Complexes"],
                "{{CMBSiteInfo|name=Final Target Hideout|faction=Angel Cartel|rating=4}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n",
            ),
            "Unmatched_Phantom_Site": (
                "Unmatched Phantom Site",
                ["Combat Anomalies"],
                "{{CMBSiteInfo|name=Unmatched Phantom Site|faction=Angel Cartel|rating=3}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n",
            ),
        }
        articles_meta = {}
        for slug, (title, cats, body) in pages.items():
            (wt_dir / f"{slug}.wiki").write_text(body, encoding="utf-8")
            articles_meta[slug] = {"title": title, "wikitext_path": f"wikitext/{slug}.wiki", "categories": cats}
        (wiki_dir / "catalog.json").write_text(
            json.dumps({"summary": {"article_count": len(pages)}, "articles": articles_meta}),
            encoding="utf-8",
        )

        out_c = self.root / "g345_content.json"
        out_r = self.root / "g345_report.json"
        content_doc, report_doc = imp.run_import(wiki_dir, sde_db2, ext_json2, out_c, out_r)

        # Compact schema_version=2 check + non-empty runtime entries
        self.assertEqual(content_doc["header"]["schema_version"], 2)
        raw_content_text = out_c.read_text(encoding="utf-8")
        self.assertNotIn('\n  "', raw_content_text)

        by_eid = {e["entry_id"]: e for e in content_doc["entries"]}

        # G3: Unmatched segregation (Unmatched Phantom Site must NEVER appear in runtime entries)
        self.assertNotIn("Unmatched Phantom Site#1", by_eid)
        self.assertEqual([u["entry_id"] for u in report_doc["unmatched_entries"]], ["Unmatched Phantom Site#1"])

        # G3: 1-to-1 primary keys with L2 vs L4 separated and DED qualifier -> CONCORD Assembly (500006)
        idx_m = content_doc["indexes"]["by_mission_key"]
        idx_d = content_doc["indexes"]["by_dungeon_key"]
        self.assertEqual(idx_m["9101:500011:2:"], "Alpha Clash (Angel Cartel) (Level 2)#1")
        self.assertEqual(idx_m["9102:500011:4:"], "Alpha Clash (Angel Cartel) (Level 4)#1")
        self.assertEqual(idx_d["5001:500011:2:"], "Alpha Clash (Angel Cartel) (Level 2)#1")
        self.assertEqual(idx_d["5002:500011:4:"], "Alpha Clash (Angel Cartel) (Level 4)#1")
        self.assertEqual(idx_d["5101:500011:0:part_1_of_2"], "Chain Trail#1")
        self.assertEqual(idx_d["5102:500011:0:part_2_of_2"], "Chain Trail#2")
        self.assertEqual(idx_d["5201:500006:5:"], "Crimson Complex (DED)#1")
        self.assertEqual(by_eid["Alpha Clash (Angel Cartel) (Level 2)#1"]["categories"], ["Security missions"])

        # G4: Multi-stage Chain Trail wiring + MessageBox banner filter + no phantom cmbsite unit + no dangling escalation_target_ids
        ct1 = by_eid["Chain Trail#1"]
        ct2 = by_eid["Chain Trail#2"]
        self.assertEqual(ct1["sde_matches"]["dungeon_ids"], [5101])
        self.assertEqual(ct2["sde_matches"]["dungeon_ids"], [5102])
        self.assertEqual(ct1["next_stage_entry_id"], "Chain Trail#2")
        self.assertEqual(ct1["escalation_target_ids"], ["Chain Trail#2"])
        self.assertEqual(ct2["escalation_target_ids"], ["Final Target Hideout#1"])
        self.assertIn("Unmatched Phantom Site", ct2["escalation_links"])
        self.assertNotIn("Unmatched Phantom Site#1", ct2["escalation_target_ids"])
        self.assertNotIn("On warpin: Ignore non-mission banner", ct2["escalation_links"])
        # Cross-page (1 of 2) -> (2 of 2) series
        st1 = by_eid["Storyline Series (1 of 2) (Angel Cartel)#1"]
        self.assertEqual(st1["next_stage_entry_id"], "Storyline Series (2 of 2) (Angel Cartel)#1")
        self.assertEqual(st1["escalation_target_ids"], ["Storyline Series (2 of 2) (Angel Cartel)#1"])

        # G5: Distance + unparsed flag + metadata prose moved to report + original_text intact
        l2_waves = by_eid["Alpha Clash (Angel Cartel) (Level 2)#1"]["rooms"][0]["waves"]
        self.assertEqual(l2_waves[0]["distance_m"], {"min_m": 15000, "max_m": 25000})
        self.assertIsNone(l2_waves[1]["distance_m"])
        self.assertTrue(l2_waves[1]["distance_unparsed"])
        self.assertEqual(l2_waves[1]["distance_raw"], "200+ km")
        self.assertEqual(l2_waves[0]["spawns"][0]["trigger"]["original_text"], "Last ship killed triggers next wave")
        self.assertNotIn("rewards", by_eid["Alpha Clash (Angel Cartel) (Level 2)#1"].get("metadata", {}))
        prose_entries = {p["entry_id"]: p for p in report_doc["entry_metadata_prose"]}
        self.assertEqual(prose_entries["Alpha Clash (Angel Cartel) (Level 2)#1"]["rewards"], "500 ISK")
        self.assertEqual(prose_entries["Alpha Clash (Angel Cartel) (Level 2)#1"]["extra"], "Hint L2")

        # Verify CLI --verify passes cleanly on this G3/G4/G5 synthetic corpus
        rc = imp.main(
            [
                "--wiki-dir", str(wiki_dir),
                "--sde-db", str(sde_db2),
                "--mission-ext", str(ext_json2),
                "--out-content", str(out_c),
                "--out-report", str(out_r),
                "--verify",
                "--min-resolved-percent", "100.0",
                "--min-candidate-mapped-percent", "100.0",
            ]
        )
        self.assertEqual(rc, 0)

    def test_inflected_wave_headers_aggro_notes_and_heading_reset(self) -> None:
        wikitext = (
            "{{CMBSiteInfo|name=Barracks Test|type=unrated|faction=Angel Cartel}}\n"
            "=== First Pocket ===\n"
            "{{NPCTableHead|Initial Group (aggro on arrival)}}\n"
            "{{NPCTableRow|Frigate|2|Gistii Outlaw}}\n"
            "{{NPCTableSeparator|Group 1}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "{{NPCTableSeparator|Group 2 (50km, auto-aggro on warp-in.)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Rogue}}\n"
            "{{NPCTableHead|Reinforcements (Barracks attacked)}}\n"
            "{{NPCTableRow|Frigate|2|Gistii Thug}}\n"
            "{{NPCTableHead|Reinforcements (Barracks 50% shields)}}\n"
            "{{NPCTableRow|Frigate|2|Gistii Outlaw}}\n"
            "{{NPCTableHead|Reinforcements - 50% shields}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "{{NPCTableHead|Reinforcements (Barracks destroyed)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Rogue}}\n"
            "{{NPCTableHead|Reinforcements (Commander killed)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Thug}}\n"
            "{{NPCTableHead|Reinforcements (Barracks aggressed)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n"
            "{{NPCTableHead|Reinforcements (aggressing Tower)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "{{NPCTableHead|Reinforcements (Gate approached)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Rogue}}\n"
            "{{NPCTableHead|Reinforcements (Barracks 50 % structure)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Thug}}\n"
            "{{NPCTableHead|Reinforcements (Triggered by Core Admiral)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n"
            "{{NPCTableHead|Reinforcements (Core Admiral destroyed)}}\n"
            "{{NPCTableSeparator|Group 1 (upon Sentry pulse)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "{{NPCTableHead|Initial Structures (Barracks attacked)}}\n"
            "{{NPCTableSeparator|Structures}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Rogue}}\n"
            "==== Wave 3 ====\n"
            "{{NPCTableSeparator|Group A}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "|} \n"
        )
        parsed = imp.parse_wiki_page("Barracks Test", ["Unrated Complexes"], wikitext, self.sde)
        self.assertIsNotNone(parsed)
        waves = parsed["units"][0]["rooms"][0]["waves"]
        self.assertEqual(len(waves), 16)

        # Initial head + two separators preserve initial_spawn / simultaneous_initial_group with inferred links
        self.assertEqual(waves[0]["timing"], "initial_spawn")
        self.assertIsNone(waves[0]["wave_trigger"])
        self.assertEqual(waves[0]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(waves[0]["wave_link"]["inference_rule"], "table_head_initial")
        for sep_idx in (1, 2):
            self.assertEqual(waves[sep_idx]["timing"], "simultaneous_initial_group")
            self.assertIsNone(waves[sep_idx]["wave_trigger"])
            self.assertEqual(waves[sep_idx]["wave_link"]["classification_state"], "inferred")
            self.assertEqual(waves[sep_idx]["wave_link"]["inference_rule"], "table_separator_initial_group")

        # Inflected, plural shield, damage, aggress, destroy/kill, and approach header triggers parse explicitly
        expected_header_triggers = [
            (3, "this_ship_destroyed", "on_attack"),
            (4, "this_ship_destroyed", "on_damage"),
            (5, "this_ship_destroyed", "on_damage"),
            (6, "this_ship_destroyed", "on_destroy"),
            (7, "this_ship_destroyed", "on_destroy"),
            (8, "this_ship_destroyed", "on_attack"),
            (9, "this_ship_destroyed", "on_attack"),
            (10, "ship_approached", "on_destroy"),
            (11, "this_ship_destroyed", "on_damage"),
        ]
        for idx, exp_kind, exp_cond in expected_header_triggers:
            self.assertEqual(waves[idx]["timing"], "sequential_wave", f"wave {idx}")
            self.assertEqual(waves[idx]["wave_trigger"]["kind"], exp_kind, f"wave {idx}")
            self.assertEqual(waves[idx]["wave_trigger"]["condition"], exp_cond, f"wave {idx}")
            self.assertEqual(waves[idx]["wave_trigger"]["classification_state"], "explicit", f"wave {idx}")
            self.assertEqual(waves[idx]["wave_link"]["classification_state"], "explicit", f"wave {idx}")

        # Verb-less passive trigger paren remains unclassified (no default guessing)
        self.assertEqual(waves[12]["timing"], "sequential_wave")
        self.assertIsNone(waves[12]["wave_trigger"]["kind"])
        self.assertEqual(waves[12]["wave_trigger"]["classification_state"], "unclassified")
        self.assertEqual(waves[12]["wave_link"]["classification_state"], "unclassified")

        # Non-None unclassified separator paren inherits classified parent head trigger instead of shadowing it
        pulse_trig = imp.classify_wave_header_trigger("Group 1 (upon Sentry pulse)")
        assert pulse_trig is not None
        self.assertEqual(pulse_trig["classification_state"], "unclassified")
        self.assertFalse(imp._is_sequential_separator_label("Group 1 (upon Sentry pulse)"))
        self.assertEqual(waves[13]["timing"], "sequential_wave")
        self.assertEqual(waves[13]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertEqual(waves[13]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(waves[13]["wave_link"]["classification_state"], "explicit")

        # Combat warships under a static separator beneath an initial head WITH a classified trigger stay sequential
        self.assertEqual(waves[14]["timing"], "sequential_wave")
        self.assertEqual(waves[14]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertEqual(waves[14]["wave_trigger"]["condition"], "on_attack")
        self.assertEqual(waves[14]["wave_trigger"]["classification_state"], "explicit")

        # Outgoing mid-paren clauses, tank/positional notes, and unclassified initial triggers stay initial/static without leaking wave_trigger
        admiral_trig = imp.classify_wave_header_trigger("Initial Group (Triggered by Core Admiral)")
        assert admiral_trig is not None
        self.assertEqual(admiral_trig["classification_state"], "unclassified")
        for non_incoming_hdr in (
            "Initial Group (15km, triggers wave 2)",
            "Initial Group (auto-aggro, triggers reinforcements)",
            "Initial Defenders (Armor tank)",
            "Initial Defenders (near structure)",
            "Initial Defenders (on left)",
        ):
            self.assertIsNone(imp.classify_wave_header_trigger(non_incoming_hdr), non_incoming_hdr)
        for non_trigger_note in ("Armor tank", "Near structure", "On left"):
            tr = imp.classify_trigger(non_trigger_note)
            assert tr is not None
            self.assertEqual(tr["classification_state"], "unclassified", non_trigger_note)
            self.assertIsNone(tr["kind"], non_trigger_note)
            self.assertIsNone(tr["condition"], non_trigger_note)

        header_edge_wiki = (
            "{{CMBSiteInfo|name=Header Edge Site|type=unrated|faction=Angel Cartel}}\n"
            "=== Pocket 1 ===\n"
            "{{NPCTableHead|Initial Group (Triggered by Core Admiral)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n"
            "{{NPCTableHead|Initial Group (15km, triggers wave 2)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Bandit}}\n"
            "{{NPCTableHead|Initial Defenders (Armor tank)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Rogue}}\n"
            "{{NPCTableHead|Initial Defenders (on left)}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Thug}}\n"
            "{{NPCTableHead|Sentry Towers (upon Sentry pulse)}}\n"
            "{{NPCTableRow|Sentry|1|Angel Cruise Missile Battery}}\n"
            "{{NPCTableHead|Sentry Towers (Triggered by destroying Bunker)}}\n"
            "{{NPCTableRow|Sentry|1|Angel Heavy Missile Battery}}\n"
            "|}\n"
        )
        edge_parsed = imp.parse_wiki_page("Header Edge Site", ["Unrated Complexes"], header_edge_wiki, self.sde)
        assert edge_parsed is not None
        ewaves = edge_parsed["units"][0]["rooms"][0]["waves"]
        self.assertEqual(len(ewaves), 6)
        for idx in range(4):
            self.assertEqual(ewaves[idx]["timing"], "initial_spawn", f"edge wave {idx}")
            self.assertIsNone(ewaves[idx]["wave_trigger"], f"edge wave {idx}")
            self.assertEqual(ewaves[idx]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(ewaves[4]["timing"], "static_structures")
        self.assertIsNone(ewaves[4]["wave_trigger"])
        self.assertEqual(ewaves[4]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(ewaves[5]["timing"], "sequential_wave")
        self.assertIsNotNone(ewaves[5]["wave_trigger"])
        self.assertEqual(ewaves[5]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(ewaves[5]["wave_trigger"]["kind"], "this_ship_destroyed")

        # Heading wave (from_heading=True) resets stale head state so subsequent separator is not initial
        self.assertEqual(waves[15]["timing"], "sequential_wave")
        self.assertEqual(waves[15]["wave_link"]["classification_state"], "unclassified")

        # Stage boundaries reset variant_heading_level so Stage 2 Wave headings stay waves (not variants)
        stage_reset_wiki = (
            "{{CMBSiteInfo|name=Stage Reset Expedition|type=expedition|faction=Angel Cartel}}\n"
            "== First location ==\n"
            "=== Variant 1 ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
            "=== Variant 2 ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Bandit}}\n|}\n"
            "== Second location ==\n"
            "=== Wave 1 ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Rogue}}\n|}\n"
            "=== Wave 2 ===\n"
            "{{NPCTableHead|Wave 2}}\n{{NPCTableRow|Frigate|1|Gistii Thug}}\n|}\n"
        )
        reset_parsed = imp.parse_wiki_page("Stage Reset Expedition", ["Expeditions"], stage_reset_wiki, self.sde)
        assert reset_parsed is not None
        self.assertEqual(len(reset_parsed["units"]), 3)
        self.assertEqual(reset_parsed["units"][0]["part_info"], {"part": 1, "of": 2})
        self.assertEqual(reset_parsed["units"][1]["part_info"], {"part": 1, "of": 2})
        self.assertEqual(reset_parsed["units"][2]["part_info"], {"part": 2, "of": 2})
        self.assertEqual(len(reset_parsed["units"][2]["rooms"][0]["waves"]), 2)
        self.assertIsNone(reset_parsed["units"][2]["variant_name"])

        # Independent variants in both Stage 1 and Stage 2 get isolated per-stage variant counts
        dual_variant_wiki = (
            "{{CMBSiteInfo|name=Split Expedition|type=expedition|faction=Angel Cartel}}\n"
            "== First location ==\n"
            "=== Variant 1 ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
            "=== Variant 2 ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Bandit}}\n|}\n"
            "== Second location ==\n"
            "=== Alpha Variant ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Rogue}}\n|}\n"
            "=== Beta Variant ===\n"
            "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Thug}}\n|}\n"
        )
        dual_parsed = imp.parse_wiki_page("Split Expedition", ["Expeditions"], dual_variant_wiki, self.sde)
        assert dual_parsed is not None
        self.assertEqual(len(dual_parsed["units"]), 4)
        self.assertEqual(
            [(u["part_info"], u["variant_index"], u["variant_count"]) for u in dual_parsed["units"]],
            [
                ({"part": 1, "of": 2}, 1, 2),
                ({"part": 1, "of": 2}, 2, 2),
                ({"part": 2, "of": 2}, 1, 2),
                ({"part": 2, "of": 2}, 2, 2),
            ],
        )

        # Encounter objective mentioning noun 'mine' ('mine field' or non-ore SDE structure 'Outpost') never extracts mine_ore or mining payload
        for obj_text, exp_kind in (
            ("Destroy the pirate mine field and clear defenders.", "kill_all"),
            ("Destroy the mine outpost and clear defenders.", "destroy_structure"),
        ):
            mine_enc = imp.parse_wiki_page(
                "Pirate Mine Sweeper (Angel Cartel)",
                ["Security missions"],
                f"{{{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective={obj_text}}}}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
                self.sde,
            )
            assert mine_enc is not None
            mine_obj = mine_enc["units"][0]["objective"]
            self.assertEqual(mine_obj["kind"], exp_kind)
            self.assertEqual(mine_obj["classification_state"], "explicit")
            self.assertIsNone(mine_obj["inference_rule"])
            self.assertIsNone(mine_enc["units"][0]["mining"])

        # R4 BF-01: Initial Structures header overridden by combat warship must drop unclassified header trigger
        ws_init_wiki = (
            "{{CMBSiteInfo|name=Warship Initial Test|faction=Angel Cartel}}\n"
            "{{NPCTableHead|Initial Structures (Triggered by Core Admiral)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "|}\n"
        )
        ws_init = imp.parse_wiki_page("Warship Initial Test", ["Combat Anomalies"], ws_init_wiki, self.sde)
        assert ws_init is not None
        ws_wave = ws_init["units"][0]["rooms"][0]["waves"][0]
        self.assertEqual(ws_wave["timing"], "initial_spawn")
        self.assertIsNone(ws_wave["wave_trigger"])
        self.assertEqual(ws_wave["wave_link"]["classification_state"], "inferred")
        self.assertEqual(ws_wave["wave_link"]["inference_rule"], "table_head_initial")

        # R4 BF-02: per-segment aggro stripping preserves explicit triggers in same paren
        for hdr, exp_cond in (
            ("Wave 2 (auto-aggro, on destroying Control Tower)", "on_destroy"),
            ("Wave 2 (auto-aggro, Barracks attacked)", "on_attack"),
        ):
            tr = imp.classify_wave_header_trigger(hdr)
            assert tr is not None
            self.assertEqual(tr["classification_state"], "explicit", hdr)
            self.assertEqual(tr["kind"], "this_ship_destroyed", hdr)
            self.assertEqual(tr["condition"], exp_cond, hdr)
        self.assertIsNone(imp.classify_wave_header_trigger("Wave 2 (15km auto-aggro)"))

        # R4 BF-03: unparenthesized fallback drops skipped non-trigger parentheticals
        tr_shields = imp.classify_wave_header_trigger("Reinforcements - 50% shields (on warp-in)")
        assert tr_shields is not None
        self.assertEqual(tr_shields["classification_state"], "explicit")
        self.assertEqual(tr_shields["kind"], "this_ship_destroyed")
        self.assertEqual(tr_shields["condition"], "on_damage")
        self.assertNotIn("warp-in", tr_shields["original_text"].lower())

        tr_timed = imp.classify_wave_header_trigger("Wave 2 - after 2 minutes (orbit 15km)")
        assert tr_timed is not None
        self.assertEqual(tr_timed["classification_state"], "explicit")
        self.assertEqual(tr_timed["kind"], "timed")
        self.assertEqual(tr_timed["time_seconds"], 120)

        tr_uncls = imp.classify_wave_header_trigger("Wave 2 - triggered by Core Admiral (50km range)")
        assert tr_uncls is not None
        self.assertEqual(tr_uncls["classification_state"], "unclassified")
        self.assertIsNone(tr_uncls["kind"])

        tr_last = imp.classify_wave_header_trigger("Group 2 - last ship of the group (orbit 15km)")
        assert tr_last is not None
        self.assertEqual(tr_last["classification_state"], "explicit")
        self.assertEqual(tr_last["kind"], "last_in_group_destroyed")

        # R4 BF-04: verb-before-layer damage triggers match this_ship_destroyed + on_damage
        for dmg_txt in ("Damaging the Barracks shield", "Breaking armor", "Hitting shields"):
            dmg_tr = imp.classify_trigger(dmg_txt)
            assert dmg_tr is not None
            self.assertEqual(dmg_tr["classification_state"], "explicit", dmg_txt)
            self.assertEqual(dmg_tr["kind"], "this_ship_destroyed", dmg_txt)
            self.assertEqual(dmg_tr["condition"], "on_damage", dmg_txt)

        # R4 BF-05: Location 6+ parses exact numeric part instead of clamping to 1
        loc6_wiki = (
            "{{CMBSiteInfo|name=Six Stage Site|type=expedition|faction=Angel Cartel}}\n"
            + "\n".join(
                f"== Location {i} ==\n{{{{NPCTableHead|Initial}}}}\n{{{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}}}\n|}}"
                for i in range(1, 7)
            )
        )
        loc6 = imp.parse_wiki_page("Six Stage Site", ["Expeditions"], loc6_wiki, self.sde)
        assert loc6 is not None
        self.assertEqual([u["part_info"] for u in loc6["units"]], [{"part": i, "of": 6} for i in range(1, 7)])

        # R4 P2-04: ordinals/non-numeric slash labels parse cleanly while numeric ranges reject + flag unparsed
        self.assertEqual(imp.parse_wave_distance("Group 1 (50km to 1st gate)"), {"min_m": 50000, "max_m": 50000})
        self.assertEqual(imp.parse_wave_distance("Initial (30km / auto-aggro)"), {"min_m": 30000, "max_m": 30000})
        for ambig in ("Wave 2 (40/80km)", "Wave 2 (50km to 80km)", "Wave 2 (50km ± 10km)"):
            self.assertIsNone(imp.parse_wave_distance(ambig), ambig)
        dist_page = imp.parse_wiki_page(
            "Distance Labels Test",
            ["Combat Anomalies"],
            "{{CMBSiteInfo|name=Distance Labels Test|faction=Angel Cartel}}\n"
            "{{NPCTableHead|Group 1 (50km to 1st gate)}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n|}\n"
            "{{NPCTableHead|Initial (30km / auto-aggro)}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n|}\n"
            "{{NPCTableHead|Wave 2 (50km ± 10km)}}\n{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n|}\n",
            self.sde,
        )
        assert dist_page is not None
        dwaves = dist_page["units"][0]["rooms"][0]["waves"]
        self.assertEqual(dwaves[0]["distance_m"], {"min_m": 50000, "max_m": 50000})
        self.assertFalse(dwaves[0].get("distance_unparsed", False))
        self.assertEqual(dwaves[1]["distance_m"], {"min_m": 30000, "max_m": 30000})
        self.assertFalse(dwaves[1].get("distance_unparsed", False))
        self.assertIsNone(dwaves[2]["distance_m"])
        self.assertTrue(dwaves[2]["distance_unparsed"])

    def test_stage_ordinals_part_isolation_courier_disambig_and_escalations(self) -> None:
        sde_db3 = self.root / "reg_sde.sqlite"
        ext_json3 = self.root / "reg_ext.json"
        conn = sqlite3.connect(str(sde_db3))
        conn.executescript(
            """
            CREATE TABLE sde_categories (category_id INTEGER PRIMARY KEY, name TEXT);
            CREATE TABLE sde_groups (group_id INTEGER PRIMARY KEY, category_id INTEGER, name TEXT);
            CREATE TABLE sde_types (
                type_id INTEGER PRIMARY KEY, group_id INTEGER, category_id INTEGER,
                name TEXT, faction_id INTEGER, volume REAL
            );
            CREATE TABLE sde_dungeons (
                dungeon_id INTEGER PRIMARY KEY, name TEXT, archetype_id INTEGER,
                faction_id INTEGER, difficulty INTEGER
            );
            CREATE TABLE sde_missions (
                mission_id INTEGER PRIMARY KEY, name TEXT, is_courier INTEGER,
                lp_alpha INTEGER, lp_omega INTEGER, has_standing_rewards INTEGER
            );
            """
        )
        conn.executemany("INSERT INTO sde_categories VALUES (?, ?)", [(11, "Entity"), (16, "Commodity")])
        conn.executemany("INSERT INTO sde_groups VALUES (?, ?, ?)", [(601, 11, "Deadspace Angel Cartel Frigate")])
        conn.executemany(
            "INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?)",
            [
                (1001, 601, 11, "Gistii Outlaw", 500011, 20.0),
                (29570, 526, 16, "Data Chip", None, 1.0),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)",
            [
                (6101, "Ordinal Expedition", 38, 500011, None),
                (6102, "Ordinal Expedition", 38, 500011, None),
                (6103, "Ordinal Expedition", 38, 500011, None),
                (6104, "Ordinal Expedition", 38, 500011, None),
                (7102, "Unrelated Side Quest", 22, 500011, 2),
                (7104, "Target Multi-Part", 22, 500011, 2),
                (7105, "Other Stage Hijack", 22, 500011, 2),
                (8102, "Dual Title Duty", 22, 500011, 2),
                (9101, "Site A", 38, 500011, None),
                (9102, "Site B", 38, 500011, None),
                (9103, "Site C", 38, 500011, None),
                (9201, "Split Expedition", 38, 500011, None),
                (9202, "Split Expedition", 38, 500011, None),
                (9301, "Teaser Variant Site", 38, 500011, None),
                (9402, "Warship Override Site", 38, 500011, None),
            ],
        )
        conn.executemany(
            "INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?)",
            [
                (7001, "Target Mission (1 of 2)", 0, 100, 150, 1),
                (7002, "Unrelated Side Quest", 0, 100, 150, 1),
                (7003, "Target Multi-Part", 0, 100, 150, 1),
                (7004, "Other Stage Hijack", 0, 100, 150, 1),
                (8001, "Dual Title Duty", 1, 100, 150, 1),
                (8002, "Dual Title Duty", 0, 100, 150, 1),
                (8011, "Mixed Series (1 of 2)", 1, 100, 150, 1),
                (8012, "Mixed Series (2 of 2)", 0, 100, 150, 1),
                (8021, "Agent Chain (1 of 2)", 0, 100, 150, 1),
                (9501, "Disjoint Chain (1 of 2)", 0, 100, 150, 1),
                (9502, "Disjoint Chain (2 of 2)", 0, 100, 150, 1),
            ],
        )
        conn.commit()
        conn.close()
        ext_json3.write_text(
            json.dumps({"mission_dungeons": {"7002": 7102, "7003": 7104, "7004": 7105, "8002": 8102}}),
            encoding="utf-8",
        )

        articles = [
            # Ordinal 1st..4th location splitting & 1-to-1 ascending dungeon mapping
            (
                "Ordinal Expedition",
                ["Expeditions"],
                "{{CMBSiteInfo|name=Ordinal Expedition|type=expedition|faction=Angel Cartel}}\n"
                "== 1st location ==\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== 2nd location ==\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== 3rd location==\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== 4th location ==\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # Part-qualified (1 of 2) mission with section heading matching another SDE mission/dungeon
            (
                "Target Mission (1 of 2) (Angel Cartel)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Clear outpost.}}\n"
                "== Unrelated Side Quest ==\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # Part 1 - <other mission> stage heading must NOT hijack
            (
                "Target Multi-Part",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Clear part 1.}}\n"
                "== Part 1 - Other Stage Hijack ==\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # Courier sharing title with combat mission matches courier (no combat dungeon)
            (
                "Dual Title Duty",
                ["Distribution missions"],
                "{{Missiondetails|type=Courier|level=2|faction1=Angel Cartel|objective=Transport 1 x Data Chip (1.0 m3).}}\n",
            ),
            # Multi-target escalation ", [[B]] and [[C]]" list + cross-faction/courier (k of N)
            (
                "Site A",
                ["Combat Anomalies"],
                "{{CMBSiteInfo|name=Site A|type=anomaly|faction=Angel Cartel}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== Escalation ==\nCan escalate to [[Site B]], [[Site C]] and [[Site B]].\n",
            ),
            ("Site B", ["Expeditions"], "{{CMBSiteInfo|name=Site B|faction=Angel Cartel}}\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"),
            ("Site C", ["Expeditions"], "{{CMBSiteInfo|name=Site C|faction=Angel Cartel}}\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"),
            (
                "Mixed Series (1 of 2)",
                ["Distribution missions"],
                "{{Missiondetails|type=Courier|level=2|objective=Deliver 1 x Data Chip (1.0 m3).}}\n",
            ),
            (
                "Mixed Series (2 of 2) (Angel Cartel)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Eliminate guards.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # Agent page hosting a sub-mission under a nested (1 of 2) heading resolves section_heading fallback
            (
                "Sample Cosmos Agent",
                ["COSMOS agents", "COSMOS missions"],
                "== Arc Overview ==\n"
                "=== Agent Chain (1 of 2) ===\n"
                "{{Missiondetails|type=Encounter|level=2|faction1=Angel Cartel|objective=Stop raiders.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # Multi-stage + multi-variant expedition chains all variants & forward escalation links cleanly
            (
                "Split Expedition",
                ["Expeditions"],
                "{{CMBSiteInfo|name=Split Expedition|type=expedition|faction=Angel Cartel}}\n"
                "== First location ==\n"
                "=== Variant 1 ===\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "=== Variant 2 ===\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== Second location ==\n"
                "=== Alpha Variant ===\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "=== Beta Variant ===\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== Escalation ==\nCan escalate to [[Site B]].\n",
            ),
            # R4 P2-03: CMBSiteBox teaser attaches escalation link to ALL variants of the stage
            (
                "Teaser Variant Site",
                ["Combat Anomalies"],
                "{{CMBSiteInfo|name=Teaser Variant Site|type=anomaly|faction=Angel Cartel}}\n"
                "=== Variant 1 ===\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "=== Variant 2 ===\n{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n"
                "== Escalation ==\n{{CMBSiteBox|name=Site B}}\n",
            ),
            # R4 BF-01: full pipeline --verify covers Initial Structures + warship trigger clearing
            (
                "Warship Override Site",
                ["Combat Anomalies"],
                "{{CMBSiteInfo|name=Warship Override Site|type=anomaly|faction=Angel Cartel}}\n"
                "{{NPCTableHead|Initial Structures (Triggered by Core Admiral)}}\n"
                "{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            # R4 P2-02: disjoint non-empty faction (k of N) pages must never cross-link
            (
                "Disjoint Chain (1 of 2)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Amarr Empire|objective=Clear part 1.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
            (
                "Disjoint Chain (2 of 2)",
                ["Security missions"],
                "{{Missiondetails|type=Encounter|level=2|faction1=Minmatar Republic|objective=Clear part 2.}}\n"
                "{{NPCTableHead|Initial}}\n{{NPCTableRow|Frigate|1|Gistii Outlaw}}\n|}\n",
            ),
        ]

        wiki_dir = self.root / "reg_wiki"
        (wiki_dir / "wikitext").mkdir(parents=True)
        articles_meta = {}
        for idx, (title, cats, txt) in enumerate(articles, start=1):
            slug = f"page_{idx}"
            fname = f"{slug}.wiki"
            (wiki_dir / "wikitext" / fname).write_text(txt, encoding="utf-8")
            articles_meta[slug] = {"title": title, "categories": cats, "wikitext_path": f"wikitext/{fname}"}
        (wiki_dir / "catalog.json").write_text(
            json.dumps({"summary": {"article_count": len(articles_meta)}, "articles": articles_meta}),
            encoding="utf-8",
        )

        out_c = self.root / "reg_content.json"
        out_r = self.root / "reg_report.json"
        content_doc, report_doc = imp.run_import(wiki_dir, sde_db3, ext_json3, out_c, out_r)
        by_eid = {e["entry_id"]: e for e in content_doc["entries"]}

        # Ordinal multi-stage expeditions split & map 1-to-1
        for stage in range(1, 5):
            st_entry = by_eid[f"Ordinal Expedition#{stage}"]
            self.assertEqual(st_entry["part_info"], {"part": stage, "of": 4})
            self.assertEqual(st_entry["sde_matches"]["dungeon_ids"], [6100 + stage])
            if stage < 4:
                self.assertEqual(st_entry["next_stage_entry_id"], f"Ordinal Expedition#{stage + 1}")
        self.assertEqual(content_doc["indexes"]["by_dungeon_key"]["6103:500011:0:part_3_of_4"], "Ordinal Expedition#3")

        # Part-qualified titles stay isolated from unrelated headings
        tm1 = by_eid["Target Mission (1 of 2) (Angel Cartel)#1"]
        self.assertEqual(tm1["sde_matches"]["mission_ids"], [7001])
        self.assertEqual(tm1["sde_matches"]["dungeon_ids"], [])
        tmp1 = by_eid["Target Multi-Part#1"]
        self.assertEqual(tmp1["sde_matches"]["mission_ids"], [7003])
        self.assertEqual(tmp1["sde_matches"]["dungeon_ids"], [7104])
        self.assertEqual(by_eid["Sample Cosmos Agent#1"]["sde_matches"]["mission_ids"], [8021])

        # Courier missions stay matched even when sharing a name with combat missions
        dual = by_eid["Dual Title Duty#1"]
        self.assertEqual(dual["sde_matches"]["mission_ids"], [8001])
        self.assertEqual(dual["sde_matches"]["dungeon_ids"], [])

        # Escalation lists, cross-type series links, multi-variant stage chains, and report totals invariants
        self.assertEqual(sorted(by_eid["Site A#1"]["escalation_target_ids"]), ["Site B#1", "Site C#1"])
        self.assertEqual(by_eid["Mixed Series (1 of 2)#1"]["next_stage_entry_id"], "Mixed Series (2 of 2) (Angel Cartel)#1")
        for s1_id in ("Split Expedition#1", "Split Expedition#2"):
            self.assertEqual(by_eid[s1_id]["next_stage_entry_id"], "Split Expedition#3")
            self.assertIn("Split Expedition#3", by_eid[s1_id]["escalation_target_ids"])
        for s2_id in ("Split Expedition#3", "Split Expedition#4"):
            self.assertEqual(by_eid[s2_id]["escalation_target_ids"], ["Site B#1"])
        for tv_id in ("Teaser Variant Site#1", "Teaser Variant Site#2"):
            self.assertEqual(by_eid[tv_id]["escalation_target_ids"], ["Site B#1"])
        self.assertEqual(by_eid["Warship Override Site#1"]["rooms"][0]["waves"][0]["timing"], "initial_spawn")
        self.assertIsNone(by_eid["Warship Override Site#1"]["rooms"][0]["waves"][0]["wave_trigger"])
        self.assertNotIn("next_stage_entry_id", by_eid["Disjoint Chain (1 of 2)#1"])
        self.assertNotIn("escalation_target_ids", by_eid["Disjoint Chain (1 of 2)#1"])
        self.assertEqual(
            content_doc["indexes"]["by_dungeon_key"]["9201:500011:0:part_1_of_2__Variant 1"],
            "Split Expedition#1",
        )
        self.assertEqual(
            content_doc["indexes"]["by_dungeon_key"]["9201:500011:0:part_1_of_2__Variant 2"],
            "Split Expedition#2",
        )
        t = report_doc["totals"]
        self.assertEqual(sum(t["wave_link_classification_state_counts"].values()), t["waves_total"])
        self.assertEqual(t["waves_with_unparsed_distance"], t["unparsed_distances_total"])
        self.assertEqual(sum(t["runtime_wave_link_classification_state_counts"].values()), t["runtime_waves_total"])
        self.assertEqual(
            t["runtime_wave_link_classification_state_counts"].get("unclassified", 0),
            t["runtime_waves_unclassified"],
        )

        rc = imp.main(
            [
                "--wiki-dir", str(wiki_dir),
                "--sde-db", str(sde_db3),
                "--mission-ext", str(ext_json3),
                "--out-content", str(out_c),
                "--out-report", str(out_r),
                "--verify",
                "--min-resolved-percent", "100.0",
                "--min-candidate-mapped-percent", "100.0",
            ]
        )
        self.assertEqual(rc, 0)

    def test_track_b_fixed_regression_matrix_no_default_guessing(self) -> None:
        trigger_matrix = [
            ("Triggers Reinforcements spawn", "this_ship_destroyed", "explicit", "on_destroy", None),
            ("Aggressing triggers 2nd wave", "this_ship_destroyed", "explicit", "on_attack", None),
            ("Spawns backup when shields drop to 50%", "this_ship_destroyed", "explicit", "on_damage", None),
            ("Approach within 15 km triggers wave 2", "ship_approached", "explicit", "on_destroy", None),
            ("Spawns after 2 minutes", "timed", "explicit", "on_destroy", 120),
            ("Last ship in group triggers Wave 2", "last_in_group_destroyed", "explicit", "on_destroy", None),
            ("Last battleship spawns reinforcement", "last_of_class_destroyed", "explicit", "on_destroy", None),
            ("Destroying completes the mission", "objective_complete", "explicit", "on_destroy", None),
            ("Destroying may trigger escalation", None, "unclassified", None, None),
            ("Possible escalation trigger", None, "unclassified", None, None),
            ("Time triggered?", None, "unclassified", None, None),
            ("1st Reinforcement", None, "unclassified", None, None),
            ("A random ship of this group is the trigger", None, "unclassified", None, None),
        ]
        for raw, exp_kind, exp_state, exp_cond, exp_sec in trigger_matrix:
            got = imp.classify_trigger(raw)
            self.assertIsNotNone(got, raw)
            assert got is not None
            self.assertEqual(got["kind"], exp_kind, raw)
            self.assertEqual(got["classification_state"], exp_state, raw)
            self.assertIsNone(got["inference_rule"], raw)
            self.assertEqual(got["original_text"], raw, raw)
            self.assertEqual(got.get("condition"), exp_cond, raw)
            self.assertEqual(got.get("time_seconds"), exp_sec, raw)

        header_matrix = [
            ("Reinforcements (Spawns the next wave when attacked)", None, None, None, None),
            ("Initial Group (Spawns reinforcements when attacked)", None, None, None, None),
            ("Wave 2 (Spawns when Barracks destroyed)", "this_ship_destroyed", "explicit", "on_destroy", None),
            ("Wave 2 (Spawns after 2 minutes)", "timed", "explicit", "on_destroy", 120),
            ("Wave 2 (Trigger: Barracks destroyed)", "this_ship_destroyed", "explicit", "on_destroy", None),
            ("Wave 2 (50km and 80km, Trigger: Gate approached)", "ship_approached", "explicit", "on_destroy", None),
            ("Timed Reinforcements - 3 minutes after warp-in", "timed", "explicit", "on_destroy", 180),
            ("Initial Group (10 km from trigger)", None, None, None, None),
            ("Initial Group (50km, auto-aggro, triggers reinforcements)", None, None, None, None),
            ("Timed Reinforcements - Some time after warp-in", None, None, None, None),
            ("Wave 2 (Triggered by Core Admiral)", None, "unclassified", None, None),
        ]
        for hdr, exp_kind, exp_state, exp_cond, exp_sec in header_matrix:
            got = imp.classify_wave_header_trigger(hdr)
            if exp_state is None:
                self.assertIsNone(got, hdr)
            else:
                self.assertIsNotNone(got, hdr)
                assert got is not None
                self.assertEqual(got["kind"], exp_kind, hdr)
                self.assertEqual(got["classification_state"], exp_state, hdr)
                self.assertEqual(got.get("condition"), exp_cond, hdr)
                self.assertEqual(got.get("time_seconds"), exp_sec, hdr)

        unrecognized_objectives = [
            "Destroy",
            "Follow the mysterious drone",
            "Confiscated viral Agent (20.0 m3)",
            "Some completely unrecognized wiki phrasing",
        ]
        for raw_obj in unrecognized_objectives:
            got = imp.build_structured_objective(
                {"objective": raw_obj, "mission_type": ""},
                None,
                None,
                [],
                {},
                self.sde,
                [],
            )
            self.assertIsNone(got["kind"], raw_obj)
            self.assertEqual(got["classification_state"], "unclassified", raw_obj)
            self.assertIsNone(got["inference_rule"], raw_obj)
            self.assertEqual(got["original_text"], raw_obj, raw_obj)

    def test_track_b_g1_f01_f02_f04_and_trigger_wiring_provenance(self) -> None:
        wikitext = (
            "{{CMBSiteInfo|name=Trigger Matrix Site|type=unrated|faction=Serpentis}}\n"
            "=== First Pocket ===\n"
            "{{NPCTableHead|Wave 1 (Barracks attacked)}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n"
            "{{NPCTableSeparator|Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Reinforcements (Spawns the next wave when attacked)}}\n"
            "{{NPCTableRow|Cruiser|2|Synth Corelatis Squad Leader}}\n"
            "=== Second Pocket ===\n"
            "{{NPCTableHead|Wave 1}}\n"
            "{{NPCTableSeparator|Group 1 (20 km)}}\n"
            "{{NPCTableRow|Battlecruiser|1|Synth Corelatis Squad Leader/Wing Leader|trigger=Wing Leader destruction triggers Wave 4}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider|trigger=Triggers wave 2, possible escalation?}}\n"
            "{{NPCTableSeparator|Reinforcements spawn triggered by attacking above}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Ambusher}}\n"
            "{{NPCTableSeparator|Group 2 (25 km)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableSeparator|Wave 4}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{NPCTableSeparator|Lootable Structures}}\n"
            "{{NPCTableRow|Structure|1|Angel Bunker}}\n"
            "=== Third Pocket ===\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher|trigger=Spawns next wave}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Outlaw|trigger=Triggers 2nd Reinforcement}}\n"
            "{{NPCTableHead|Ambush Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Backup Wave}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Squad Leader}}\n"
            "{{NPCTableHead|Timed Reinforcements - 3 minutes after warp-in}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "|}\n"
        )
        parsed = imp.parse_wiki_page("Trigger Matrix Site", ["Unrated Complexes"], wikitext, self.sde)
        self.assertIsNotNone(parsed)
        assert parsed is not None
        rooms = parsed["units"][0]["rooms"]
        self.assertEqual(len(rooms), 3)

        # Room 1: sequential Wave 2 never inherits Wave 1 trigger; outgoing "Spawns the next wave when attacked" on Reinforcements stays non-trigger (unclassified incoming link);
        # zero default last_in_group_destroyed anywhere.
        r1_waves = rooms[0]["waves"]
        self.assertEqual(r1_waves[0]["timing"], "sequential_wave")
        self.assertEqual(r1_waves[0]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertIsNone(r1_waves[1]["wave_trigger"])
        self.assertEqual(r1_waves[1]["wave_link"]["classification_state"], "unclassified")
        self.assertIsNone(r1_waves[1]["wave_link"]["trigger_kind"])
        self.assertIsNone(r1_waves[2]["wave_trigger"])
        self.assertEqual(r1_waves[2]["wave_link"]["classification_state"], "unclassified")
        self.assertIsNone(r1_waves[2]["wave_link"]["trigger_kind"])

        # Room 2: first_combat_wave_initial promotion, F-04 A/B narrowing, uncertain trigger excluded, and F-01 sub-separator non-leakage onto Group 2 & Lootable Structures
        r2_waves = rooms[1]["waves"]
        self.assertEqual(r2_waves[0]["timing"], "initial_spawn")
        self.assertEqual(r2_waves[0]["wave_link"]["inference_rule"], "first_combat_wave_initial")
        tr_wl = r2_waves[0]["spawns"][0]["trigger"]
        self.assertEqual(tr_wl["trigger_ship_tag"], "r2_w1_s1")
        self.assertEqual(tr_wl["trigger_ship_name"], "Synth Corelatis Wing Leader")
        self.assertEqual(tr_wl["trigger_type_ids"], [10004])
        self.assertEqual(tr_wl["target_wave_index"], 4)
        self.assertEqual(tr_wl["classification_state"], "explicit")

        # Uncertain trigger must remain unclassified and unwired
        tr_unc = r2_waves[0]["spawns"][1]["trigger"]
        self.assertIsNone(tr_unc["kind"])
        self.assertEqual(tr_unc["classification_state"], "unclassified")
        self.assertNotIn("target_wave_index", tr_unc)

        # Reinforced wave stays explicit from header; Group 2 remains unlinked (no F-01 leakage); Wave 4 receives ONLY Wing Leader trigger; Lootable Structures stays static_structures
        self.assertEqual(r2_waves[1]["wave_link"]["classification_state"], "explicit")
        self.assertIsNone(r2_waves[2]["wave_trigger"])
        self.assertEqual(r2_waves[2]["wave_link"]["classification_state"], "unclassified")
        self.assertEqual(r2_waves[3]["wave_trigger"]["trigger_ship_tag"], "r2_w1_s1")
        self.assertEqual(r2_waves[3]["wave_link"]["classification_state"], "explicit")
        self.assertEqual(r2_waves[4]["timing"], "static_structures")
        self.assertIsNone(r2_waves[4]["wave_trigger"])

        # Room 3: explicit spawn trigger stays explicit on source row while wave_link/wave_trigger record inferred linking rules & 180s timed header trigger
        r3_waves = rooms[2]["waves"]
        tr_next = r3_waves[0]["spawns"][0]["trigger"]
        self.assertEqual(tr_next["target_wave_index"], 2)
        self.assertEqual(tr_next["classification_state"], "explicit")
        self.assertEqual(r3_waves[1]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(r3_waves[1]["wave_link"]["inference_rule"], "immediate_next_combat_wave")
        tr_pos = r3_waves[0]["spawns"][1]["trigger"]
        self.assertEqual(tr_pos["classification_state"], "explicit")
        self.assertEqual(tr_pos["target_wave_index"], 3)
        self.assertEqual(r3_waves[2]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(r3_waves[2]["wave_link"]["inference_rule"], "positional_reinforcement_offset")
        self.assertEqual(r3_waves[3]["wave_trigger"]["kind"], "timed")
        self.assertEqual(r3_waves[3]["wave_trigger"]["time_seconds"], 180)
        self.assertEqual(r3_waves[3]["wave_link"]["classification_state"], "explicit")

    def test_track_b_g2_all_twelve_objective_kinds_three_state(self) -> None:
        self.assertEqual(len(imp.OBJECTIVE_KIND_ENUM), 12)
        observed_kinds: set[str] = set()

        cases = [
            ("kill_all", "explicit", None, {"objective": "Eliminate all pirates in the pocket", "mission_type": "Encounter"}, None, None, [], {}),
            ("kill_target", "explicit", None, {"objective": "Eliminate the Synth Pithatis Enforcer guarding the gate", "mission_type": "Encounter"}, None, None, [], {}),
            ("destroy_structure", "explicit", None, {"objective": "Destroy the Angel Bunker near the gate", "mission_type": "Encounter"}, None, None, [], {}),
            ("retrieve_item", "explicit", None, {"objective": "Bring 2x Crate of Synth Supplies (40 m3) to your agent", "mission_type": "Trade"}, None, None, [], {}),
            ("deliver_item", "explicit", None, {"objective": "Deliver 1x Synth Supplies to the station", "mission_type": "Courier"}, None, None, [], {}),
            ("mine_ore", "inferred", "missiondetails_mining_payload", {"objective": "Mine Synthium Ore", "mission_type": "Mining"}, None, {"ore_or_material": "Synthium Ore", "ore_type_ids": [28618], "quantity": 100, "volume_m3": 30.0}, [], {}),
            ("approach", "explicit", None, {"objective": "Approach the ancient acceleration gate to scout the area", "mission_type": "Encounter"}, None, None, [], {}),
            ("hack", "explicit", None, {"objective": "Hack the Serpentis mainframe", "mission_type": "Data"}, None, None, [], {}),
            ("manufacture", "explicit", None, {"objective": "Manufacture 1x Synth Supplies", "mission_type": "Storyline"}, None, None, [], {}),
            ("talk_to_agent", "explicit", None, {"objective": "Report to Abdiel Verat at K-QWHE V - Moon 4", "mission_type": "Storyline"}, None, None, [], {}),
            ("branch_choice", "explicit", None, {"objective": "Choose your path between the two factions", "mission_type": "Storyline"}, None, None, [], {}),
            ("travel", "explicit", None, {"objective": "Travel to the Amarr destination system", "mission_type": "Storyline"}, None, None, [], {}),
        ]
        for exp_kind, exp_state, exp_rule, meta, courier, mining, cats, sde_match in cases:
            got = imp.build_structured_objective(meta, courier, mining, [], sde_match, self.sde, cats)
            self.assertEqual(got["kind"], exp_kind, meta)
            self.assertEqual(got["classification_state"], exp_state, meta)
            self.assertEqual(got["inference_rule"], exp_rule, meta)
            observed_kinds.add(got["kind"])

        self.assertEqual(observed_kinds, set(imp.OBJECTIVE_KIND_ENUM))

        # Check inferred courier payload + site-type + category + spawn_objective_complete_trigger
        inf_courier = imp.build_structured_objective(
            {"objective": "", "mission_type": "Courier"},
            {"item": "Crate of Synth Supplies", "item_type_ids": [29570], "quantity": 1, "volume_m3": 20.0},
            None,
            [],
            {},
            self.sde,
            [],
        )
        self.assertEqual(inf_courier["kind"], "deliver_item")
        self.assertEqual(inf_courier["classification_state"], "inferred")
        self.assertEqual(inf_courier["inference_rule"], "missiondetails_courier_payload")

        inf_site = imp.build_structured_objective(
            {"objective": "", "site_type": "Data"},
            None,
            None,
            [],
            {},
            self.sde,
            [],
        )
        self.assertEqual(inf_site["kind"], "hack")
        self.assertEqual(inf_site["inference_rule"], "cmbsite_data_relic_type")

        inf_cat = imp.build_structured_objective(
            {"objective": "", "site_type": ""},
            None,
            None,
            [],
            {},
            self.sde,
            ["Relic Sites"],
        )
        self.assertEqual(inf_cat["kind"], "hack")
        self.assertEqual(inf_cat["inference_rule"], "wiki_data_relic_category")

        inf_spawn_rooms = [
            {
                "room_index": 1,
                "waves": [
                    {
                        "wave_index": 1,
                        "spawns": [
                            {
                                "raw_class": "Structure",
                                "raw_name": "Angel Bunker",
                                "count": {"min": 1, "max": 1},
                                "resolution": {"type_ids": [16757]},
                                "trigger": {
                                    "kind": "objective_complete",
                                    "classification_state": "explicit",
                                    "original_text": "Destroying completes the mission",
                                },
                            }
                        ],
                    }
                ],
            }
        ]
        inf_spawn = imp.build_structured_objective(
            {"objective": "", "mission_type": "Encounter"},
            None,
            None,
            inf_spawn_rooms,
            {},
            self.sde,
            [],
        )
        self.assertEqual(inf_spawn["kind"], "destroy_structure")
        self.assertEqual(inf_spawn["classification_state"], "inferred")
        self.assertEqual(inf_spawn["inference_rule"], "spawn_objective_complete_trigger")
        self.assertEqual(inf_spawn["target_type_ids"], [16757])

    def test_track_b_overrides_validation_determinism_and_tracked_files(self) -> None:
        # 1. Tracked repo overrides and ratchet files must pass strict schema validation
        repo_overrides = imp.load_overrides(imp.default_overrides_path())
        self.assertEqual(repo_overrides["schema_version"], 1)
        self.assertGreater(len(repo_overrides["entries"]), 0)
        repo_ratchet = imp.load_coverage_ratchet(imp.default_coverage_ratchet_path())
        self.assertEqual(repo_ratchet["schema_version"], 1)

        # 2. Invalid overrides docs rejected cleanly
        bad_docs = [
            {"schema_version": 2, "entries": {}},
            {"schema_version": 1, "entries": {"E#1": {"objective": {"kind": "not_a_kind"}}}},
            {"schema_version": 1, "entries": {"E#1": {"objective": {"kind": "kill_all", "classification_state": "inferred"}}}},
            {"schema_version": 1, "entries": {"E#1": {"objective": {"kind": "kill_all", "classification_state": "explicit", "inference_rule": "nope"}}}},
            {"schema_version": 1, "entries": {"E#1": {"waves": {"0": {"timing": "sequential_wave"}}}}},
            {"schema_version": 1, "entries": {"E#1": {"waves": {"1": {"timing": "initial_spawn", "trigger_kind": "timed"}}}}},
            {"schema_version": 1, "entries": {"E#1": {"waves": {"1": {"spawns": {"1": {"kind": "bad_trig"}}}}}}},
        ]
        for bad in bad_docs:
            with self.assertRaises(ValueError, msg=repr(bad)):
                imp.validate_overrides_doc(bad)

        # 3. Deterministic post-parse application: preserves original_text, syncs objective_kind,
        # marks override_applied only on modified targets (not Wave 1 when only Wave 1 spawn is modified),
        # propagates time_seconds onto target wave, and records unmatched entries/targets.
        wikitext = (
            "{{Missiondetails\n"
            "|Type=Encounter\n"
            "|Objective=Confiscated item crate\n"
            "}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher|trigger=1st Reinforcement}}\n"
            "{{NPCTableHead|1st Reinforcement}}\n"
            "{{NPCTableRow|Frigate|2|Synth Gistii Raider}}\n"
            "|}\n"
        )
        parsed = imp.parse_wiki_page("Override Sample Mission", ["Level 1 Missions"], wikitext, self.sde)
        assert parsed is not None
        unit = parsed["units"][0]
        self.assertEqual(unit["objective"]["classification_state"], "unclassified")
        self.assertEqual(unit["objective"]["original_text"], "Confiscated item crate")
        self.assertIsNone(unit["rooms"][0]["waves"][1]["wave_trigger"])

        ovr = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {
                    "Override Sample Mission#1": {
                        "objective": {
                            "kind": "retrieve_item",
                            "classification_state": "explicit",
                            "target_name": "Crate of Synth Supplies",
                            "item_quantity": 1,
                        },
                        "waves": {
                            "1": {
                                "spawns": {
                                    "1": {
                                        "kind": "timed",
                                        "time_seconds": 45,
                                        "target_wave_index": 2,
                                        "classification_state": "explicit",
                                    }
                                }
                            }
                        },
                    },
                    "Missing Entry#9": {"objective": {"kind": "kill_all"}},
                    "Override Sample Mission#1_bad": {"objective": {"kind": "kill_all"}},
                },
            }
        )
        summary = imp.apply_overrides([unit], ovr)
        self.assertEqual(summary["applied_entries"], 1)
        self.assertEqual(summary["applied_objectives"], 1)
        self.assertEqual(summary["applied_waves"], 0)
        self.assertEqual(summary["applied_spawns"], 1)
        self.assertEqual(len(summary["unmatched_entries"]), 2)
        self.assertEqual(unit["objective"]["kind"], "retrieve_item")
        self.assertEqual(unit["objective"]["original_text"], "Confiscated item crate")
        self.assertTrue(unit["objective"]["override_applied"])
        self.assertEqual(unit["objective_kind"], "retrieve_item")
        # Wave 1 itself was NOT overridden, only its spawn 1 -> Wave 1 wave_link keeps override_applied absent
        self.assertNotIn("override_applied", unit["rooms"][0]["waves"][0]["wave_link"])
        sp_trig = unit["rooms"][0]["waves"][0]["spawns"][0]["trigger"]
        self.assertTrue(sp_trig["override_applied"])
        self.assertEqual(sp_trig["original_text"], "1st Reinforcement")
        self.assertEqual(sp_trig["time_seconds"], 45)
        tgt_wave = unit["rooms"][0]["waves"][1]
        self.assertEqual(tgt_wave["wave_trigger"]["kind"], "timed")
        self.assertEqual(tgt_wave["wave_trigger"]["time_seconds"], 45)
        self.assertTrue(tgt_wave["wave_trigger"]["override_applied"])
        self.assertEqual(tgt_wave["wave_link"]["classification_state"], "explicit")
        self.assertEqual(tgt_wave["wave_link"]["trigger_kind"], "timed")
        self.assertTrue(tgt_wave["wave_link"]["override_applied"])

    def test_track_b_coverage_metrics_and_ratchet_enforcement(self) -> None:
        tmp_dir = Path(tempfile.mkdtemp(prefix="wiki_pve_t2_cov_"))
        self.addCleanup(lambda: __import__("shutil").rmtree(tmp_dir, ignore_errors=True))
        sde_db = tmp_dir / "sde.sqlite"
        ext_json = tmp_dir / "ext.json"
        _build_synthetic_sde(sde_db, ext_json)
        wiki_dir = tmp_dir / "wiki"
        wiki_dir.mkdir()
        (wiki_dir / "Manifest.txt").write_text("Synthetic Supply Run\nSynthetic Multi-Part (1 of 3)\n", encoding="utf-8")

        (wiki_dir / "Synthetic Supply Run.wiki").write_text(
            "{{Missiondetails\n"
            "|Type=Courier\n"
            "|Objective=Deliver 1x Crate of Synth Supplies\n"
            "}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "|}\n",
            encoding="utf-8",
        )
        (wiki_dir / "Synthetic Multi-Part (1 of 3).wiki").write_text(
            "{{Missiondetails\n"
            "|Type=Encounter\n"
            "|Objective=Destroy\n"
            "}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher|trigger=Spawns next wave}}\n"
            "{{NPCTableHead|Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Wave 3}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "|}\n",
            encoding="utf-8",
        )

        out_c = tmp_dir / "content.json"
        out_r = tmp_dir / "report.json"
        overrides_path = tmp_dir / "overrides.json"
        overrides_path.write_text(json.dumps({"schema_version": 1, "entries": {}}), encoding="utf-8")

        _, report = imp.run_import(
            wiki_dir=wiki_dir,
            sde_db_path=sde_db,
            mission_ext_path=ext_json,
            out_content_path=out_c,
            out_report_path=out_r,
            overrides_path=overrides_path,
            apply_default_overrides=False,
        )
        cov = report["coverage"]
        # 2 missions: 1 inferred (deliver_item via missiondetails_courier_payload), 1 unclassified ("Destroy") -> 50.0%
        self.assertEqual(cov["g2_objectives"]["missions"]["total"], 2)
        self.assertEqual(cov["g2_objectives"]["missions"]["inferred_count"], 1)
        self.assertEqual(cov["g2_objectives"]["missions"]["unclassified_count"], 1)
        self.assertEqual(cov["g2_objectives"]["missions"]["structured_percent"], 50.0)
        # 4 waves total: 2 initial (inferred), 1 triggered by spawn (inferred), 1 unannotated Wave 3 (unclassified) -> 75.0%
        self.assertEqual(cov["g1_triggers"]["wave_links"]["total"], 4)
        self.assertEqual(cov["g1_triggers"]["wave_links"]["inferred_count"], 3)
        self.assertEqual(cov["g1_triggers"]["wave_links"]["unclassified_count"], 1)
        self.assertEqual(cov["g1_triggers"]["wave_links"]["classified_percent"], 75.0)
        self.assertEqual(cov["g1_triggers"]["spawn_triggers_target_wired_percent"], 100.0)

        ratchet_path = tmp_dir / "ratchet.json"
        # Small-corpus guard refuses default min_corpus_pages=100
        with self.assertRaises(ValueError):
            imp.tighten_coverage_ratchet(report, ratchet_path)
        # Tighten with explicit min_corpus_pages creates monotonic floors from current run
        tightened = imp.tighten_coverage_ratchet(report, ratchet_path, min_corpus_pages=2)
        self.assertEqual(tightened["floors"]["g2_missions_structured_percent"], 50.0)
        self.assertEqual(tightened["floors"]["g1_wave_links_classified_percent"], 75.0)

        # CLI --verify passes with exact ratchet
        with contextlib.redirect_stdout(io.StringIO()):
            rc_ok = imp.main(
                [
                    "--wiki-dir", str(wiki_dir),
                    "--sde-db", str(sde_db),
                    "--mission-ext", str(ext_json),
                    "--out-content", str(out_c),
                    "--out-report", str(out_r),
                    "--overrides", str(overrides_path),
                    "--coverage-ratchet", str(ratchet_path),
                    "--verify",
                    "--min-resolved-percent", "100.0",
                    "--min-candidate-mapped-percent", "100.0",
                ]
            )
        self.assertEqual(rc_ok, 0)

        # Raising floor higher than actual must fail check_coverage_ratchet, CLI --verify, and tighten_coverage_ratchet
        higher_ratchet = dict(tightened)
        higher_ratchet["floors"] = dict(tightened["floors"])
        higher_ratchet["floors"]["g2_missions_structured_percent"] = 99.0
        ratchet_path.write_text(json.dumps(higher_ratchet), encoding="utf-8")

        errs = imp.check_coverage_ratchet(report, higher_ratchet)
        self.assertEqual(len(errs), 1)
        self.assertIn("g2_missions_structured_percent", errs[0])

        with self.assertRaises(ValueError):
            imp.tighten_coverage_ratchet(report, ratchet_path)

        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            rc_fail = imp.main(
                [
                    "--wiki-dir", str(wiki_dir),
                    "--sde-db", str(sde_db),
                    "--mission-ext", str(ext_json),
                    "--out-content", str(out_c),
                    "--out-report", str(out_r),
                    "--overrides", str(overrides_path),
                    "--coverage-ratchet", str(ratchet_path),
                    "--verify",
                    "--min-resolved-percent", "100.0",
                    "--min-candidate-mapped-percent", "100.0",
                ]
            )
        self.assertEqual(rc_fail, 1)

    def test_track_b_r2_bf01_through_bf05_and_p2_regressions(self) -> None:
        # BF-01: Outgoing "Spawns ..." notes remain initial/unclassified while incoming "Spawns when/after..." classifies.
        wiki_bf01 = (
            "{{Missiondetails\n|Type=Encounter\n|Objective=Destroy all hostiles\n}}\n"
            "{{NPCTableHead|Initial Group (Spawns reinforcements when attacked)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{NPCTableHead|Wave 2 (Spawns when Barracks destroyed)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Wave 3 (Spawns after 2 minutes)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "|}\n"
        )
        p_bf01 = imp.parse_wiki_page("Outgoing Header Site", ["Level 1 Missions"], wiki_bf01, self.sde)
        assert p_bf01 is not None
        bf01_waves = p_bf01["units"][0]["rooms"][0]["waves"]
        self.assertEqual(bf01_waves[0]["timing"], "initial_spawn")
        self.assertIsNone(bf01_waves[0]["wave_trigger"])
        self.assertEqual(bf01_waves[1]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertEqual(bf01_waves[1]["wave_trigger"]["condition"], "on_destroy")
        self.assertEqual(bf01_waves[2]["wave_trigger"]["kind"], "timed")
        self.assertEqual(bf01_waves[2]["wave_trigger"]["time_seconds"], 120)

        # BF-02: Hair-Trigger maps to on_attack, and committed overrides match wiki semantics.
        for trig_txt in ("Hair-Trigger", "1A Reinforcement (Hair-Trigger)"):
            ct = imp.classify_trigger(trig_txt)
            self.assertEqual(ct["kind"], "this_ship_destroyed", trig_txt)
            self.assertEqual(ct["condition"], "on_attack", trig_txt)
            self.assertEqual(ct["classification_state"], "explicit", trig_txt)

        repo_ovr = imp.load_overrides(imp.default_overrides_path())
        self.assertNotIn("Lafuni Oduntra#2", repo_ovr["entries"])
        halt_obj = repo_ovr["entries"]["Halt the Invasion (Minmatar Republic)#1"]["objective"]
        self.assertEqual(halt_obj["kind"], "destroy_structure")
        self.assertEqual(halt_obj["target_name"], "Minmatar Occupied Stargate")
        self.assertEqual(halt_obj["target_type_ids"], [93960])
        lunuin_obj = repo_ovr["entries"]["Lunuin Eurek#2"]["objective"]
        self.assertEqual(lunuin_obj["kind"], "kill_target")
        self.assertEqual(lunuin_obj["target_name"], "Abufyr Joek")
        self.assertEqual(lunuin_obj["target_type_ids"], [26119])
        self.assertEqual(
            repo_ovr["entries"]["Guristas Strike - Final Battle#1"]["objective"]["kind"],
            "destroy_structure",
        )
        mh_sp = repo_ovr["entries"]["The Mordus Headhunters (Level 5)#1"]["rooms"]["1"]["waves"]["1"]["spawns"]
        self.assertEqual(mh_sp["3"]["condition"], "on_attack")
        self.assertEqual(mh_sp["4"]["condition"], "on_attack")

        # BF-04: Positional Wave N uses combat_seq + positional_wave_index, respects earlier label guard,
        # and spawn links replace unclassified header wave_trigger.
        wiki_bf04 = (
            "{{CMBSiteInfo|name=Positional Wave Site|type=unrated|faction=Serpentis}}\n"
            "{{NPCTableHead|Static Structures}}\n"
            "{{NPCTableRow|Structure|1|Angel Bunker}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher|trigger=Triggers Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider|trigger=Triggers Backup Wave}}\n"
            "{{NPCTableHead|Backup Wave (Triggered by Synth Gistii Raider)}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Squad Leader}}\n"
            "{{NPCTableHead|Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Outlaw|trigger=Triggers Wave 2}}\n"
            "|}\n"
        )
        p_bf04 = imp.parse_wiki_page("Positional Wave Site", ["Unrated Complexes"], wiki_bf04, self.sde)
        assert p_bf04 is not None
        bf04_waves = p_bf04["units"][0]["rooms"][0]["waves"]
        # Spawn 2 in wave 2 ("Initial Group") triggers wave 3 ("Backup Wave") explicitly, replacing unclassified header trigger
        self.assertEqual(bf04_waves[2]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(bf04_waves[2]["wave_trigger"]["source_wave_index"], 2)
        self.assertEqual(bf04_waves[2]["wave_trigger"]["source_spawn_index"], 2)
        # Wave 4 ("Wave 2") self-triggering "Triggers Wave 2" is blocked by earlier_label / forward-only guard
        self.assertNotIn("target_wave_index", bf04_waves[3]["spawns"][0]["trigger"])

        wiki_bf04_pos = (
            "{{CMBSiteInfo|name=Positional CombatSeq Site|type=unrated|faction=Serpentis}}\n"
            "{{NPCTableHead|Static Structures}}\n"
            "{{NPCTableRow|Structure|1|Angel Bunker}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher|trigger=Triggers Wave 2}}\n"
            "{{NPCTableHead|Ambush Wave (Triggered by Synth Gistii Ambusher)}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Squad Leader}}\n"
            "|}\n"
        )
        p_pos = imp.parse_wiki_page("Positional CombatSeq Site", ["Unrated Complexes"], wiki_bf04_pos, self.sde)
        assert p_pos is not None
        pos_waves = p_pos["units"][0]["rooms"][0]["waves"]
        self.assertEqual(pos_waves[1]["spawns"][0]["trigger"]["target_wave_index"], 3)
        self.assertEqual(pos_waves[2]["wave_trigger"]["classification_state"], "inferred")
        self.assertEqual(pos_waves[2]["wave_trigger"]["inference_rule"], "positional_wave_index")
        self.assertEqual(pos_waves[2]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(pos_waves[2]["wave_link"]["inference_rule"], "positional_wave_index")

        # BF-05 & P2-02: Item payload extraction rejects ordinals/system names/preceding combat counts;
        # manufacture does not hijack combat_lead + build; negation guards suppress "Do not destroy/kill".
        self.assertIsNone(imp.extract_item_objective_payload("2nd pocket of pirates", self.sde))
        self.assertIsNone(imp.extract_item_objective_payload("Go to 3L-Y9M and destroy 5 ships", self.sde))
        pay_ret = imp.extract_item_objective_payload("Destroy 5 ships and retrieve 3x Reports", self.sde)
        assert pay_ret is not None
        self.assertEqual(pay_ret["quantity"], 3)
        self.assertEqual(pay_ret["item"], "Reports")

        obj_build = imp.build_structured_objective(
            {"objective": "Kill the pirates before they build a fortress", "mission_type": "Encounter"},
            None, None, [], {}, self.sde, [],
        )
        self.assertNotEqual(obj_build["kind"], "manufacture")
        obj_neg = imp.build_structured_objective(
            {"objective": "Do not destroy the warehouse", "mission_type": ""},
            None, None, [], {}, self.sde, [],
        )
        self.assertEqual(obj_neg["classification_state"], "unclassified")
        obj_mixed = imp.build_structured_objective(
            {"objective": "Do not kill the Admiral; destroy the Bunker", "mission_type": "Encounter"},
            None, None, [], {}, self.sde, [],
        )
        self.assertEqual(obj_mixed["kind"], "destroy_structure")
        self.assertEqual(obj_mixed["target_name"], "Bunker")

        # P2-04: Overrides validator rejects room_index under rooms.<r>.waves and invalid objective scalar types.
        for bad_ovr in (
            {
                "schema_version": 1,
                "entries": {"M#1": {"rooms": {"1": {"waves": {"1": {"room_index": 1, "timing": "initial_spawn"}}}}}},
            },
            {"schema_version": 1, "entries": {"M#1": {"objective": {"kind": "kill_target", "target_name": ""}}}},
            {"schema_version": 1, "entries": {"M#1": {"objective": {"kind": "deliver_item", "item_quantity": True}}}},
            {"schema_version": 1, "entries": {"M#1": {"objective": {"kind": "deliver_item", "item_quantity": 0}}}},
            {"schema_version": 1, "entries": {"M#1": {"objective": {"kind": "deliver_item", "volume_m3": -1.0}}}},
        ):
            with self.assertRaises(ValueError):
                imp.validate_overrides_doc(bad_ovr)

        # BF-03, BF-05 (content_template), P2-03 (3-char NPC token), P2-06 (small-corpus ratchet guard) via run_import & --verify
        tmp_dir = Path(tempfile.mkdtemp(prefix="wiki_pve_t2_r2_"))
        self.addCleanup(lambda: __import__("shutil").rmtree(tmp_dir, ignore_errors=True))
        sde_db2 = tmp_dir / "sde.sqlite"
        ext_json2 = tmp_dir / "ext.json"
        _build_synthetic_sde(sde_db2, ext_json2)
        conn = sqlite3.connect(str(sde_db2))
        conn.executescript(
            """
            ALTER TABLE sde_missions ADD COLUMN content_template TEXT;
            INSERT INTO sde_types VALUES (10031, 601, 11, 'Gistii Spy', NULL, 50.0);
            INSERT INTO sde_types VALUES (10032, 601, 11, 'Gistii Oni', NULL, 50.0);
            INSERT INTO sde_missions VALUES (9901, 'Synthetic Trade Template', 0, 100, 200, 1, 'agent.missionTemplatizedContent_StorylineTradeMission');
            INSERT INTO sde_missions VALUES (9902, 'Override Wave Edge Case', 0, 100, 200, 1, 'agent.missionTemplatizedContent_BasicKillMission');
            """
        )
        conn.commit()
        conn.close()

        wiki_dir2 = tmp_dir / "wiki"
        wiki_dir2.mkdir()
        (wiki_dir2 / "Manifest.txt").write_text(
            "Synthetic Trade Template\nOverride Wave Edge Case\n", encoding="utf-8"
        )
        (wiki_dir2 / "Synthetic Trade Template.wiki").write_text(
            "{{Missiondetails\n|Type=\n|Objective=\n}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Spy/Oni|trigger=Oni destruction triggers Wave 2}}\n"
            "{{NPCTableHead|Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Gistii Spy}}\n"
            "|}\n",
            encoding="utf-8",
        )
        (wiki_dir2 / "Override Wave Edge Case.wiki").write_text(
            "{{Missiondetails\n|Type=Encounter\n|Objective=Destroy\n}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Frigate|1|Synth Corelatis Squad Leader/Wing Leader|trigger=Wing Leader triggers reinforcement?}}\n"
            "{{NPCTableHead|Reinforcement Group (Triggered by mystery ship)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{NPCTableHead|Bonus Group (60 seconds after warp-in)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "|}\n",
            encoding="utf-8",
        )

        ovr2_path = tmp_dir / "ovr2.json"
        ovr2_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "entries": {
                        "Override Wave Edge Case#1": {
                            "objective": {"kind": "kill_all", "classification_state": "explicit"},
                            "waves": {
                                "1": {
                                    "spawns": {
                                        "1": {
                                            "kind": "this_ship_destroyed",
                                            "target_wave_index": 2,
                                            "classification_state": "explicit",
                                        }
                                    }
                                },
                                "3": {
                                    "trigger_kind": None,
                                },
                            },
                        }
                    },
                }
            ),
            encoding="utf-8",
        )
        out_c2 = tmp_dir / "content2.json"
        out_r2 = tmp_dir / "report2.json"
        cdoc2, rpt2 = imp.run_import(
            wiki_dir=wiki_dir2,
            sde_db_path=sde_db2,
            mission_ext_path=ext_json2,
            out_content_path=out_c2,
            out_report_path=out_r2,
            overrides_path=ovr2_path,
            apply_default_overrides=False,
        )
        by_eid2 = {e["entry_id"]: e for e in cdoc2["entries"]}
        tr_e = by_eid2["Synthetic Trade Template#1"]
        self.assertEqual(tr_e["objective"]["kind"], "retrieve_item")
        self.assertEqual(tr_e["objective"]["classification_state"], "inferred")
        self.assertEqual(tr_e["objective"]["inference_rule"], "sde_mission_template")
        oni_trig = tr_e["rooms"][0]["waves"][0]["spawns"][0]["trigger"]
        self.assertEqual(oni_trig["trigger_ship_name"], "Gistii Oni")
        self.assertEqual(oni_trig["trigger_type_ids"], [10032])

        edge_e = by_eid2["Override Wave Edge Case#1"]
        edge_waves = edge_e["rooms"][0]["waves"]
        # Narrowed trigger_type_ids preserved after parsing & applied to BOTH spawn and target wave_trigger (replacing unclassified), while _name_to_type_ids stripped
        self.assertNotIn("_name_to_type_ids", edge_waves[0]["spawns"][0]["resolution"])
        self.assertEqual(edge_waves[0]["spawns"][0]["trigger"]["trigger_ship_name"], "Synth Corelatis Wing Leader")
        self.assertEqual(edge_waves[0]["spawns"][0]["trigger"]["trigger_type_ids"], [10004])
        self.assertEqual(edge_waves[1]["wave_trigger"]["kind"], "this_ship_destroyed")
        self.assertEqual(edge_waves[1]["wave_trigger"]["trigger_ship_name"], "Synth Corelatis Wing Leader")
        self.assertEqual(edge_waves[1]["wave_trigger"]["trigger_type_ids"], [10004])
        self.assertEqual(edge_waves[1]["wave_trigger"]["classification_state"], "explicit")
        self.assertIsNone(edge_waves[2]["wave_trigger"])
        self.assertEqual(edge_waves[2]["wave_link"]["classification_state"], "unclassified")
        self.assertTrue(edge_waves[2]["wave_link"]["override_applied"])

        # Also test wave overrides: initial_spawn + trigger_kind promotion & timing-only refresh,
        # plus narrowed Wing Leader multi-ship trigger_type_ids preservation
        ovr3 = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {
                    "Override Wave Edge Case#1": {
                        "waves": {
                            "1": {"trigger_kind": "timed", "time_seconds": 30, "classification_state": "explicit"},
                            "2": {"timing": "sequential_wave"},
                        }
                    }
                },
            }
        )
        p_edge = imp.parse_wiki_page(
            "Override Wave Edge Case",
            ["Level 1 Missions"],
            (wiki_dir2 / "Override Wave Edge Case.wiki").read_text(encoding="utf-8"),
            imp.SdeIndex(sde_db2, ext_json2),
        )
        assert p_edge is not None
        sum3 = imp.apply_overrides(p_edge["units"], ovr3)
        self.assertEqual(sum3["applied_waves"], 2)
        w1_3 = p_edge["units"][0]["rooms"][0]["waves"][0]
        w2_3 = p_edge["units"][0]["rooms"][0]["waves"][1]
        self.assertEqual(w1_3["timing"], "sequential_wave")
        self.assertEqual(w1_3["wave_trigger"]["kind"], "timed")
        self.assertEqual(w1_3["wave_trigger"]["time_seconds"], 30)
        self.assertTrue(w2_3["wave_link"]["override_applied"])

        # Dangling target_wave_index & unmatched_entries fail --verify cleanly when --overrides is supplied
        ovr_bad_tgt = tmp_dir / "ovr_bad_tgt.json"
        ovr_bad_tgt.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "entries": {
                        "Override Wave Edge Case#1": {
                            "waves": {"1": {"spawns": {"1": {"kind": "this_ship_destroyed", "target_wave_index": 99}}}}
                        },
                        "Nonexistent Mission#1": {"objective": {"kind": "kill_all"}},
                    },
                }
            ),
            encoding="utf-8",
        )
        ratchet2 = tmp_dir / "ratchet2.json"
        imp.tighten_coverage_ratchet(rpt2, ratchet2, min_corpus_pages=2)
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            rc_bad_ovr = imp.main(
                [
                    "--wiki-dir", str(wiki_dir2),
                    "--sde-db", str(sde_db2),
                    "--mission-ext", str(ext_json2),
                    "--out-content", str(out_c2),
                    "--out-report", str(out_r2),
                    "--overrides", str(ovr_bad_tgt),
                    "--coverage-ratchet", str(ratchet2),
                    "--verify",
                    "--min-resolved-percent", "100.0",
                    "--min-candidate-mapped-percent", "100.0",
                ]
            )
        self.assertEqual(rc_bad_ovr, 1)

    def test_track_b_r3_bf01_bf02_bf03_and_p2_regressions(self) -> None:
        # BF-01: Spawn overrides propagate to inferred & same-source wave_triggers,
        # redirect away from old targets (clearing stale links), and yield to wave-level overrides.
        wiki_bf01 = (
            "{{Missiondetails\n|Type=Encounter\n|Objective=Eliminate all hostiles\n}}\n"
            "{{NPCTableHead|Initial Group}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Squad Leader|trigger=Triggers reinforcement}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Wing Leader|trigger=Destroying this triggers Wave 3}}\n"
            "{{NPCTableHead|Wave 2}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{NPCTableHead|Wave 3}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Wave 4}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Outlaw}}\n"
            "|}\n"
        )
        p_bf01 = imp.parse_wiki_page("BF01 Propagation Mission", ["Level 1 Missions"], wiki_bf01, self.sde)
        assert p_bf01 is not None
        waves_bf01 = p_bf01["units"][0]["rooms"][0]["waves"]
        # Pre-check: Wave 2 has inferred sequential link; Wave 3 has explicit link sourced from (1, 2)
        self.assertEqual(waves_bf01[1]["wave_trigger"]["classification_state"], "inferred")
        self.assertEqual(waves_bf01[2]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(waves_bf01[2]["wave_trigger"]["source_wave_index"], 1)
        self.assertEqual(waves_bf01[2]["wave_trigger"]["source_spawn_index"], 2)

        ovr_bf01 = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {
                    "BF01 Propagation Mission#1": {
                        "waves": {
                            "1": {
                                "spawns": {
                                    "1": {
                                        "kind": "this_ship_destroyed",
                                        "condition": "on_attack",
                                        "target_wave_index": 2,
                                        "classification_state": "explicit",
                                    },
                                    "2": {
                                        "kind": "this_ship_destroyed",
                                        "condition": "on_attack",
                                        "target_wave_index": 4,
                                        "classification_state": "explicit",
                                    },
                                }
                            }
                        }
                    }
                },
            }
        )
        sum_bf01 = imp.apply_overrides(p_bf01["units"], ovr_bf01)
        self.assertEqual(sum_bf01["applied_spawns"], 2)
        # Spawn 1 promotes inferred Wave 2 wave_trigger -> explicit on_attack
        self.assertEqual(waves_bf01[1]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(waves_bf01[1]["wave_trigger"]["condition"], "on_attack")
        self.assertEqual(waves_bf01[1]["wave_trigger"]["source_wave_index"], 1)
        self.assertEqual(waves_bf01[1]["wave_trigger"]["source_spawn_index"], 1)
        self.assertEqual(waves_bf01[1]["wave_link"]["classification_state"], "explicit")
        self.assertTrue(waves_bf01[1]["wave_link"]["override_applied"])
        # Spawn 2 redirected from Wave 3 -> Wave 4: Wave 3 stale wave_trigger cleared, Wave 4 populated
        self.assertIsNone(waves_bf01[2]["wave_trigger"])
        self.assertEqual(waves_bf01[2]["wave_link"]["classification_state"], "unclassified")
        self.assertEqual(waves_bf01[3]["wave_trigger"]["classification_state"], "explicit")
        self.assertEqual(waves_bf01[3]["wave_trigger"]["condition"], "on_attack")
        self.assertEqual(waves_bf01[3]["wave_trigger"]["source_wave_index"], 1)
        self.assertEqual(waves_bf01[3]["wave_trigger"]["source_spawn_index"], 2)

        # Same-source explicit target condition update + wave-level override precedence guard
        p_bf01_b = imp.parse_wiki_page("BF01 Precedence Mission", ["Level 1 Missions"], wiki_bf01, self.sde)
        assert p_bf01_b is not None
        waves_bf01_b = p_bf01_b["units"][0]["rooms"][0]["waves"]
        ovr_bf01_b = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {
                    "BF01 Precedence Mission#1": {
                        "waves": {
                            "1": {
                                "spawns": {
                                    "1": {
                                        "kind": "this_ship_destroyed",
                                        "condition": "on_attack",
                                        "target_wave_index": 2,
                                        "classification_state": "explicit",
                                    },
                                    "2": {
                                        "kind": "this_ship_destroyed",
                                        "condition": "on_attack",
                                        "target_wave_index": 3,
                                        "classification_state": "explicit",
                                    },
                                }
                            },
                            "2": {
                                "trigger_kind": "timed",
                                "time_seconds": 45,
                                "classification_state": "explicit",
                            },
                        }
                    }
                },
            }
        )
        imp.apply_overrides(p_bf01_b["units"], ovr_bf01_b)
        self.assertEqual(waves_bf01_b[1]["wave_trigger"]["kind"], "timed")
        self.assertEqual(waves_bf01_b[1]["wave_trigger"]["time_seconds"], 45)
        self.assertEqual(waves_bf01_b[2]["wave_trigger"]["condition"], "on_attack")
        self.assertEqual(waves_bf01_b[2]["wave_trigger"]["source_spawn_index"], 2)

        # BF-02: Validator rejects trigger mutation keys with initial/static timing or trigger_kind=None
        for bad_w in (
            {"timing": "initial_spawn", "condition": "on_attack"},
            {"timing": "static_structures", "source_wave_index": 1},
            {"trigger_kind": None, "condition": "on_attack"},
            {"trigger_kind": None, "time_seconds": 30},
            {"trigger_kind": None, "target_wave_index": 2},
        ):
            with self.assertRaises(ValueError):
                imp.validate_overrides_doc({"schema_version": 1, "entries": {"M#1": {"waves": {"1": bad_w}}}})

        # BF-02: Timing-only override preserves inferred wave_trigger & metadata;
        # partial mutation preserves classification_state & trigger_ship_*/unlocks_gate/is_escalation;
        # failed validation (missing_trigger_kind / missing_source_wave / missing_target_wave) does NOT touch wave or counters.
        p_bf02 = imp.parse_wiki_page("BF02 Preservation Mission", ["Level 1 Missions"], wiki_bf01, self.sde)
        assert p_bf02 is not None
        waves_bf02 = p_bf02["units"][0]["rooms"][0]["waves"]
        waves_bf02[1]["wave_trigger"].update(
            {
                "trigger_ship_tag": "w1_s1",
                "trigger_ship_name": "Synth Corelatis Squad Leader",
                "trigger_type_ids": [10003],
                "unlocks_gate": True,
                "is_escalation": True,
            }
        )
        ovr_bf02_timing = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {"BF02 Preservation Mission#1": {"waves": {"2": {"timing": "sequential_wave"}}}},
            }
        )
        sum_timing = imp.apply_overrides(p_bf02["units"], ovr_bf02_timing)
        self.assertEqual(sum_timing["applied_waves"], 1)
        self.assertEqual(waves_bf02[1]["timing"], "sequential_wave")
        self.assertEqual(waves_bf02[1]["wave_trigger"]["classification_state"], "inferred")
        self.assertEqual(waves_bf02[1]["wave_trigger"]["inference_rule"], "immediate_next_combat_wave")
        self.assertTrue(waves_bf02[1]["wave_trigger"]["unlocks_gate"])
        self.assertTrue(waves_bf02[1]["wave_trigger"]["is_escalation"])
        self.assertEqual(waves_bf02[1]["wave_trigger"]["trigger_type_ids"], [10003])

        ovr_bf02_partial = imp.validate_overrides_doc(
            {
                "schema_version": 1,
                "entries": {"BF02 Preservation Mission#1": {"waves": {"2": {"condition": "on_attack"}}}},
            }
        )
        imp.apply_overrides(p_bf02["units"], ovr_bf02_partial)
        self.assertEqual(waves_bf02[1]["wave_trigger"]["condition"], "on_attack")
        self.assertEqual(waves_bf02[1]["wave_trigger"]["classification_state"], "inferred")
        self.assertTrue(waves_bf02[1]["wave_trigger"]["unlocks_gate"])
        self.assertTrue(waves_bf02[1]["wave_trigger"]["is_escalation"])
        self.assertEqual(waves_bf02[1]["wave_trigger"]["trigger_ship_name"], "Synth Corelatis Squad Leader")

        # missing_trigger_kind & missing_source_wave & missing_target_wave leave wave untouched and applied_waves=0
        wiki_untrig = (
            "{{CMBSiteInfo|name=BF02 Failure Guard|type=unrated|faction=Serpentis}}\n"
            "{{NPCTableHead|Static Structures}}\n"
            "{{NPCTableRow|Structure|1|Angel Bunker}}\n"
            "{{NPCTableHead|Wave 2 (Triggered by mystery ship)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "|}\n"
        )
        for bad_w_spec, expected_reason in (
            ({"timing": "sequential_wave", "condition": "on_attack"}, "missing_trigger_kind"),
            ({"trigger_kind": "this_ship_destroyed", "source_wave_index": 99}, "missing_source_wave"),
            ({"trigger_kind": "this_ship_destroyed", "source_wave_index": 1, "source_spawn_index": 99}, "missing_source_spawn"),
            ({"trigger_kind": "this_ship_destroyed", "target_wave_index": 99}, "missing_target_wave"),
        ):
            p_fail = imp.parse_wiki_page("BF02 Failure Guard", ["Unrated Complexes"], wiki_untrig, self.sde)
            assert p_fail is not None
            w2_fail = p_fail["units"][0]["rooms"][0]["waves"][1]
            orig_timing = w2_fail["timing"]
            ovr_fail = imp.validate_overrides_doc(
                {"schema_version": 1, "entries": {"BF02 Failure Guard#1": {"waves": {"2": bad_w_spec}}}}
            )
            sum_fail = imp.apply_overrides(p_fail["units"], ovr_fail)
            self.assertEqual(sum_fail["applied_waves"], 0, expected_reason)
            self.assertEqual(sum_fail["applied_entries"], 0, expected_reason)
            self.assertFalse(w2_fail["wave_link"].get("override_applied", False), expected_reason)
            self.assertEqual(w2_fail["timing"], orig_timing, expected_reason)
            self.assertEqual(sum_fail["unmatched_targets"][0]["reason"], expected_reason)

        # BF-03: Spatial spawn parentheticals and ordinal outgoing spawn notes do not become incoming wave_triggers
        wiki_bf03 = (
            "{{Missiondetails\n|Type=Encounter\n|Objective=Destroy pirates\n}}\n"
            "{{NPCTableHead|Initial Group (Spawns 2nd wave when attacked)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Ambusher}}\n"
            "{{NPCTableSeparator|Group 2 (spawns 20 km)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Raider}}\n"
            "{{NPCTableHead|Initial Group 2 (Spawns first reinforcement on destruction)}}\n"
            "{{NPCTableRow|Frigate|1|Synth Gistii Outlaw}}\n"
            "{{NPCTableHead|Wave 2 (Spawns after 45 seconds)}}\n"
            "{{NPCTableRow|Cruiser|1|Synth Corelatis Squad Leader}}\n"
            "|}\n"
        )
        p_bf03 = imp.parse_wiki_page("BF03 Spatial And Ordinal Site", ["Level 1 Missions"], wiki_bf03, self.sde)
        assert p_bf03 is not None
        bf03_waves = p_bf03["units"][0]["rooms"][0]["waves"]
        self.assertEqual(bf03_waves[0]["timing"], "initial_spawn")
        self.assertIsNone(bf03_waves[0]["wave_trigger"])
        self.assertEqual(bf03_waves[1]["timing"], "simultaneous_initial_group")
        self.assertIsNone(bf03_waves[1]["wave_trigger"])
        self.assertEqual(bf03_waves[1]["wave_link"]["classification_state"], "inferred")
        self.assertEqual(bf03_waves[1]["wave_link"]["inference_rule"], "table_separator_initial_group")
        self.assertEqual(bf03_waves[2]["timing"], "initial_spawn")
        self.assertIsNone(bf03_waves[2]["wave_trigger"])
        self.assertEqual(bf03_waves[3]["timing"], "sequential_wave")
        self.assertEqual(bf03_waves[3]["wave_trigger"]["kind"], "timed")
        self.assertEqual(bf03_waves[3]["wave_trigger"]["time_seconds"], 45)
        self.assertEqual(bf03_waves[3]["wave_trigger"]["classification_state"], "explicit")

        # R2 P2: m_noun preposition lookahead & new delivery verbs
        pay_deliv = imp.extract_item_objective_payload("Deliver Secret Documents to agent", self.sde)
        assert pay_deliv is not None
        self.assertEqual(pay_deliv["item"], "Secret Documents")
        self.assertIsNone(imp.extract_item_objective_payload("Move to the acceleration gate", self.sde))
        self.assertIsNone(imp.extract_item_objective_payload("Deliver to Agent", self.sde))

        # R2 P2: _match_target_in_rooms does not match partial 1-token resolved row over unresolved multi-token target row
        rooms_p2 = [
            {
                "room_index": 1,
                "waves": [
                    {
                        "wave_index": 1,
                        "spawns": [
                            {"raw_name": "Synth Occupied Escort", "resolution": {"type_ids": [10004]}},
                            {"raw_name": "Minmatar Occupied Station", "resolution": {"type_ids": []}},
                        ],
                    }
                ],
            }
        ]
        obj_p2 = imp.build_structured_objective(
            {"objective": "Destroy Minmatar Occupied Station", "mission_type": "Encounter"},
            None, None, rooms_p2, {}, self.sde, [],
        )
        self.assertEqual(obj_p2["kind"], "destroy_structure")
        self.assertEqual(obj_p2["target_name"], "Minmatar Occupied Station")
        self.assertEqual(obj_p2.get("target_type_ids") or [], [])

        # R2 P2: NaN / Inf floats rejected cleanly in overrides and coverage ratchet validators
        for bad_f in (float("nan"), float("inf"), float("-inf")):
            with self.assertRaises(ValueError):
                imp.validate_overrides_doc(
                    {"schema_version": 1, "entries": {"M#1": {"objective": {"kind": "deliver_item", "volume_m3": bad_f}}}}
                )
            with self.assertRaises(ValueError):
                imp.validate_coverage_ratchet_doc(
                    {
                        "schema_version": 1,
                        "min_corpus_pages": 1,
                        "floors": {
                            "g1_wave_links_classified_percent": bad_f,
                        },
                    }
                )

        # R2 P2: --tighten-ratchet --verify leaves ratchet file untouched when --verify fails
        tmp_dir_r3 = Path(tempfile.mkdtemp(prefix="wiki_pve_t2_r3_"))
        self.addCleanup(lambda: __import__("shutil").rmtree(tmp_dir_r3, ignore_errors=True))
        sde_r3 = tmp_dir_r3 / "sde.sqlite"
        ext_r3 = tmp_dir_r3 / "ext.json"
        _build_synthetic_sde(sde_r3, ext_r3)
        wiki_r3 = tmp_dir_r3 / "wiki"
        wiki_r3.mkdir()
        (wiki_r3 / "Manifest.txt").write_text("BF03 Spatial And Ordinal Site\n", encoding="utf-8")
        (wiki_r3 / "BF03 Spatial And Ordinal Site.wiki").write_text(wiki_bf03, encoding="utf-8")
        ratchet_r3 = tmp_dir_r3 / "ratchet_r3.json"
        ratchet_init = {
            "schema_version": 1,
            "min_corpus_pages": 1,
            "floors": {
                "g1_wave_links_classified_percent": 50.0,
                "g1_wave_links_explicit_percent": 50.0,
                "g1_triggers_classified_percent": 50.0,
                "g1_spawn_triggers_target_wired_percent": 0.0,
                "g2_missions_structured_percent": 50.0,
                "g2_all_units_structured_percent": 50.0,
                "g4_npc_concrete_resolved_percent": 50.0,
                "g4_npc_candidate_mapped_percent": 50.0,
                "g5_waves_with_distance_m_percent": 0.0,
            },
        }
        ratchet_before = json.dumps(ratchet_init, indent=2) + "\n"
        ratchet_r3.write_text(ratchet_before, encoding="utf-8")
        bad_ovr_r3 = tmp_dir_r3 / "bad_ovr_r3.json"
        bad_ovr_r3.write_text(
            json.dumps({"schema_version": 1, "entries": {"Missing Page#1": {"objective": {"kind": "kill_all"}}}}),
            encoding="utf-8",
        )
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            rc_r3 = imp.main(
                [
                    "--wiki-dir", str(wiki_r3),
                    "--sde-db", str(sde_r3),
                    "--mission-ext", str(ext_r3),
                    "--out-content", str(tmp_dir_r3 / "c.json"),
                    "--out-report", str(tmp_dir_r3 / "r.json"),
                    "--overrides", str(bad_ovr_r3),
                    "--coverage-ratchet", str(ratchet_r3),
                    "--tighten-ratchet",
                    "--verify",
                    "--min-resolved-percent", "100.0",
                    "--min-candidate-mapped-percent", "100.0",
                ]
            )
        self.assertEqual(rc_r3, 1)
        self.assertEqual(ratchet_r3.read_text(encoding="utf-8"), ratchet_before)


if __name__ == "__main__":
    unittest.main()
