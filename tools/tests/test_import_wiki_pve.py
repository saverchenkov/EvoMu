"""Unit tests for tools/import_wiki_pve.py using synthetic in-memory/temp fixtures."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
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
        (630, 11, "Deadspace Serpentis Battlecruiser"),
        (319, 11, "Large Collidable Structure"),
        (366, 2, "Acceleration Gate"),
        (25, 6, "Frigate"),
        (459, 25, "Harvestable Ore"),
        (314, 17, "Commodities"),
        (671, 11, "Mission Caldari State Frigate"),
    ]
    conn.executemany("INSERT INTO sde_groups VALUES (?, ?, ?)", groups)

    types = [
        # Ships / NPCs
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
        # Structures & Gates
        (17831, 366, 2, "Acceleration Gate", None, 1000.0),
        (16757, 319, 11, "Angel Bunker", None, 1000.0),
        (16796, 319, 11, "Guristas Bunker", None, 1000.0),
        (17605, 319, 11, "Angel Stasis Tower", None, 1000.0),
        (17607, 319, 11, "Guristas Stasis Tower", None, 1000.0),
        # Ore & Courier Commodity
        (28618, 459, 25, "Synthium Ore", None, 0.3),
        (29570, 314, 17, "Crate of Synth Supplies", None, 20.0),
    ]
    conn.executemany("INSERT INTO sde_types VALUES (?, ?, ?, ?, ?, ?)", types)

    dungeons = [
        (4001, "Synthetic Raid", 4, 500010, 3),
        (4002, "Synthetic Raid", 4, 500011, 4),
        (4003, "Synthetic Multi-Part (1 of 3)", 1, 500011, 2),
    ]
    conn.executemany("INSERT INTO sde_dungeons VALUES (?, ?, ?, ?, ?)", dungeons)

    missions = [
        (9001, "Synthetic Multi-Part (1 of 3)", 0, 100, 150, 1),
        (9002, "Synthetic Supply Run", 1, 50, 75, 1),
        (9003, "Synthetic Ore Extraction", 0, 80, 120, 1),
    ]
    conn.executemany("INSERT INTO sde_missions VALUES (?, ?, ?, ?, ?, ?)", missions)

    conn.commit()
    conn.close()

    mission_ext_path.write_text(
        json.dumps({"mission_dungeons": {"9001": 4003}, "rnd_agent_skills": {}}),
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

    def test_trigger_classification_all_kinds_and_fallback(self) -> None:
        cases = [
            ("Triggers next wave", "this_ship_destroyed"),
            ("Last ship killed in group", "last_in_group_destroyed"),
            ("Last of this class destroyed", "last_of_class_destroyed"),
            ("Spawns when approached within 30km", "ship_approached"),
            ("Arrives after 2 minutes", "timed"),
            ("Destroying completes the mission objective", "objective_complete"),
            ("Mysterious cryptic whistle signal", "group_destroyed"),
        ]
        for text, expected_kind in cases:
            parsed = imp.classify_trigger(text)
            self.assertIsNotNone(parsed)
            assert parsed is not None
            self.assertEqual(parsed["kind"], expected_kind)
            self.assertEqual(parsed["original_text"], text)

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
        res_angel = self.sde.resolve_npc_row(
            npc_class="Structure",
            raw_name="Bunker",
            note="",
            page_factions=["Angel Cartel"],
        )
        self.assertEqual(res_angel["status"], "resolved_structure_faction")
        self.assertEqual(res_angel["type_ids"], [16757])

        res_guristas = self.sde.resolve_npc_row(
            npc_class="Sentry",
            raw_name="Stasis Tower",
            note="",
            page_factions=["Guristas Pirates"],
        )
        self.assertEqual(res_guristas["status"], "resolved_structure_faction")
        self.assertEqual(res_guristas["type_ids"], [17607])

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
        self.assertEqual(bunker_spawn["trigger"]["kind"], "group_destroyed")
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

        report = json.loads(out1_r.read_text(encoding="utf-8"))
        totals = report["totals"]
        self.assertEqual(totals["npc_rows_total"], 2)
        self.assertEqual(totals["npc_rows_partly_resolved"], 1)
        self.assertEqual(totals["npc_rows_unresolved"], 1)
        self.assertEqual(totals["trigger_fallback_count"], 1)
        self.assertEqual(len(report["partly_resolved_rows"]), 1)
        self.assertEqual(report["partly_resolved_rows"][0]["unresolved_parts"], ["Synth Gistii NoSuchShip"])
        self.assertEqual(len(report["unresolved_rows"]), 1)
        self.assertEqual(report["unresolved_rows"][0]["reason"], "unmatched_name_in_sde")
        self.assertEqual(len(report["trigger_fallbacks"]), 1)
        self.assertEqual(report["trigger_fallbacks"][0]["original_text"], "Mystery phrase")

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


if __name__ == "__main__":
    unittest.main()
