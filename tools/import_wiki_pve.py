#!/usr/bin/env python3
"""Import EVE University wiki PvE missions and combat sites into deterministic JSON.

Reads local MediaWiki wikitext pages from ~/ai/eve-wiki (or --wiki-dir), expands
NPCTableRow / NPCTableHead / NPCTableSeparator / Missiondetails / CMBSite* templates,
matches NPCs, structures, containers, asteroids, mission items, and dungeons against the
local SDE SQLite database + sde_agent_mission_ext.json, classifies triggers and EWAR,
and writes gitignored data/wiki_pve_content.json and data/wiki_pve_report.json.

Verification (--verify) enforces two distinct thresholds without conflating buckets:
- --min-resolved-percent (default 85.0): gates strictly concrete SDE type_ids only
  (resolved_exact + validated resolved_structure_faction). On the ~/ai/eve-wiki corpus,
  strict sde_types.name matching has an empirical ceiling ~87.1% due to blank wiki
  cells (293), bare family prefixes / '?' placeholders (201), and CCP custom dungeon
  display names absent from sde_types (994 mapped to faction/class candidates).
- --min-candidate-mapped-percent (default 90.0): gates concrete type_ids plus rows
  carrying both faction and ship class with non-empty candidate_type_ids
  (resolved_class_candidates).

IMPORTANT: Generated output contains CC BY-SA wiki-derived data and must never be committed.
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import html
import json
import os
import re
import sqlite3
import sys
import tempfile
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent

# Combat NPC rows resolve strictly against Category 11 (Entity); celestial/structure/asteroid rows
# also allow their dedicated non-ship categories in strict Entity-first priority order.
# Player Ship (6), Player Drone (18), and Player Fighter (87) categories are intentionally excluded.
ALLOWED_SDE_CATEGORIES = (2, 3, 11, 22, 23, 25, 40, 46, 65)
COMBAT_ENTITY_CATEGORIES = (11,)
GATE_ENTITY_CATEGORIES = (2, 11)
ASTEROID_ENTITY_CATEGORIES = (25, 11)
STRUCTURE_ENTITY_CATEGORIES = (11, 2, 22, 23, 3, 40, 46, 65)

SKIP_TEMPLATE_TITLES_PREFIX = (
    "Template:",
    "Module:",
    "User:",
    "Help:",
    "File:",
    "Category:",
)

FACTION_PREFIXES: dict[str, list[str]] = {
    "Amarr Empire": ["Amarr", "Imperial"],
    "Caldari State": ["Caldari", "State"],
    "Gallente Federation": ["Gallente", "Federation"],
    "Minmatar Republic": ["Minmatar", "Republic"],
    "Guristas Pirates": ["Guristas", "Dread Guristas"],
    "Angel Cartel": ["Angel", "Arch Angel", "Domination"],
    "Blood Raider Covenant": ["Blood Raider", "Bloodraider", "Blood", "Dark Blood"],
    "Sansha's Nation": ["Sansha", "True Sansha"],
    "Serpentis": ["Serpentis", "Shadow Serpentis"],
    "Rogue Drones": ["Rogue Drone", "Drone"],
    "Mordu's Legion Command": ["Mordu's", "Mordu"],
    "Mercenaries": ["Mercenary"],
    "Thukker Tribe": ["Thukker"],
    "Khanid Kingdom": ["Khanid"],
    "Ammatar Mandate": ["Ammatar"],
    "The Syndicate": ["Syndicate"],
    "CONCORD Assembly": ["CONCORD"],
    "Triglavian Collective": ["Triglavian"],
    "EDENCOM": ["EDENCOM"],
    "Drifters": ["Drifter"],
    "Equilibrium of Mankind": ["EoM"],
}

_FACTION_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = tuple(
    (re.compile(pat, re.I), canon)
    for pat, canon in (
        # Multi-word & unambiguous faction/rank stems first so short tokens never hijack them
        (r"\b(?:equilibrium\s+of\s+mankind|eom)\b", "Equilibrium of Mankind"),
        (r"\brogue\s+drones?\b", "Rogue Drones"),
        (r"\b(?:blood\s+raiders?|bloodraiders?|dark\s+blood|corpii|corpior|corpum|corpatis|corpus)\b", "Blood Raider Covenant"),
        (r"\b(?:angel\s+cartel|arch\s+angel|guardian\s+angels?|domination|gistii|gistior|gistum|gistatis|gist)\b", "Angel Cartel"),
        (r"\b(?:dread\s+guristas|guristas|pithii?|pithior|pithum|pithatis|pith)\b", "Guristas Pirates"),
        (r"\b(?:true\s+sansha|sansha(?:'s)?|centii|centior|centum|centatis|centus|centi)\b", "Sansha's Nation"),
        (
            r"\b(?:shadow\s+serpentis|serpentis|coreli|corelior|corelum|corelatis)\b"
            r"|\bcore\s+(?:admiral|baron|commodore|fleet|flotilla|grand|high|lord|port|rear|vice|serpentis|runner)\b",
            "Serpentis",
        ),
        (r"\bmordu(?:'s)?\b", "Mordu's Legion Command"),
        (r"\b(?:amarr(?:\s+empire|\s+navy)?|imperial(?:\s+navy|\s+templar|\s+war)?)\b", "Amarr Empire"),
        (r"\bcaldari(?:\s+state|\s+navy)?\b|\bstate\s+(?:military|navy|soldier|shukuro|protector|executor|war|sentry|bunker)\b|^state$", "Caldari State"),
        (r"\bgallente(?:\s+federation|\s+navy)?\b|\bfederation\b", "Gallente Federation"),
        (r"\bminmatar(?:\s+republic|\s+fleet)?\b|\brepublic\b", "Minmatar Republic"),
        (r"\bkhanid\b", "Khanid Kingdom"),
        (r"\bammatar\b", "Ammatar Mandate"),
        (r"\bthukker\b", "Thukker Tribe"),
        (r"\bsyndicate\b", "The Syndicate"),
        (r"\bconcord\b", "CONCORD Assembly"),
        (r"\btriglavian\b", "Triglavian Collective"),
        (r"\bedencom\b", "EDENCOM"),
        (r"\bdrifters?\b", "Drifters"),
        (r"\bmercenar(?:y|ies)\b", "Mercenaries"),
        # Ambiguous single-word tokens last, with strict word boundaries & negative guards
        (r"\b(?:alvi|alvior|alvum|alvu|alvatis|alvus)\b", "Rogue Drones"),
        (r"\bangel\b", "Angel Cartel"),
        (r"\bblood\b(?!\s*-|\s*(?:stained|line|bath|hound|money)\b)", "Blood Raider Covenant"),
    )
)

FACTION_WORD_PREFIXES: tuple[str, ...] = (
    "guristas", "dread guristas", "pith", "angel", "arch angel", "domination", "gist",
    "blood", "blood raider", "dark blood", "corpus", "corpior", "corpatis", "corpum", "corpii",
    "sansha", "true sansha", "centus", "centum", "centior", "centii", "centatis", "centi",
    "serpentis", "shadow serpentis", "core", "corelum", "corelior", "coreli", "corelatis",
    "rogue drone", "amarr", "imperial", "caldari", "state", "gallente", "federation",
    "minmatar", "republic", "mordu", "khanid", "ammatar", "thukker", "syndicate",
    "concord", "triglavian", "edencom", "drifter", "eom", "equilibrium of mankind",
)

CANON_FACTION_NAME_TO_ID: dict[str, int] = {
    "Caldari State": 500001,
    "Minmatar Republic": 500002,
    "Amarr Empire": 500003,
    "Gallente Federation": 500004,
    "CONCORD Assembly": 500006,
    "Ammatar Mandate": 500007,
    "Khanid Kingdom": 500008,
    "The Syndicate": 500009,
    "Guristas Pirates": 500010,
    "Angel Cartel": 500011,
    "Blood Raider Covenant": 500012,
    "ORE": 500014,
    "Thukker Tribe": 500015,
    "Servant Sisters of EVE": 500016,
    "Mordu's Legion Command": 500018,
    "Sansha's Nation": 500019,
    "Serpentis": 500020,
    "Drifters": 500024,
    "Rogue Drones": 500025,
    "Triglavian Collective": 500026,
    "EDENCOM": 500027,
}

SPELLING_PAIRS: list[tuple[str, str]] = [
    ("defence", "defense"),
    ("batery", "battery"),
    ("batteries", "battery"),
    ("neutalizer", "neutralizer"),
    ("neutrilizer", "neutralizer"),
    ("accerleration", "acceleration"),
    ("cruiser missile battery", "cruise missile battery"),
    ("cruise missile launcher", "cruise missile battery"),
    ("pithii", "pithi"),
    ("pithium ", "pithum "),
    ("centi ", "centii "),
    ("gisti ", "gistii "),
    ("gistastis ", "gistatis "),
    ("gistatsis ", "gistatis "),
    ("gistatus ", "gistatis "),
    ("gistor ", "gistior "),
    ("gistium ", "gistum "),
    ("corelli ", "coreli "),
    ("colerior ", "corelior "),
    ("corelium ", "corelum "),
    ("corpier ", "corpior "),
    ("coprii ", "corpii "),
    ("martriarch", "matriarch"),
    ("matriarc", "matriarch"),
    ("ammater ", "ammatar "),
    ("war lord", "warlord"),
    ("sentinal", "sentinel"),
    ("infilitrator", "infiltrator"),
    ("mahariel", "machariel"),
    ("watchmen", "watchman"),
    ("archangels ", "arch angel "),
    ("archangels ", "angel "),
    ("archangel ", "arch angel "),
    ("archangel", "arch angel"),
    ("blood raiders ", "blood "),
    ("blood raider ", "blood "),
    ("blood raider ", "bloodraider "),
    ("tribinus", "tribunus"),
    ("guerilla", "guerrilla"),
    ("mercanary", "mercenary"),
    ("maraudar", "marauder"),
    ("violater", "violator"),
    ("silferfish", "silverfish"),
    ("baracuda", "barracuda"),
    ("terroris", "terrorist"),
    ("gallent ", "gallente "),
    ("shatterer ", "shatter "),
    ("infestor ", "infester "),
    ("amarr navy ", "imperial navy "),
    ("gallente navy ", "federation navy "),
    ("minmatar navy ", "republic fleet "),
    ("caldari police ", "caldari state "),
    ("state ", "caldari "),
    ("caldari ", "state "),
    ("federation ", "gallente "),
    ("gallente ", "federation "),
    ("republic ", "minmatar "),
    ("minmatar ", "republic "),
    ("imperial ", "amarr "),
    ("amarr ", "imperial "),
    ("edencom aegis ", "aegis "),
    ("core runner ", ""),
    ("chain ", ""),
    ("blood escapee ", "blood "),
    ("worshiper", "worshipper"),
    ("worshipper", "worshiper"),
    ("deviner", "diviner"),
    ("patriach", "patriarch"),
    ("ursurper", "usurper"),
    ("conventer", "converter"),
    ("harbringer", "harbinger"),
    ("imperial templar ", "imperial "),
    ("divine ", "divine imperial "),
    ("’", "'"),
    ("`", "'"),
    ("aux power array", "auxiliary power array"),
    ("aux. power array", "auxiliary power array"),
    ("energy neutralizing sentry", "energy neutralizer sentry"),
    ("neuting sentry", "energy neutralizer sentry"),
    ("stasis webification battery", "stasis webifier battery"),
    ("stasis tower", "stasis webifier tower"),
]

# Pirate/empire rank metadata constrained by (faction_family, ship_class) so siblings never swap across hull sizes.
RANK_PREFIX_META: dict[str, tuple[str, str]] = {
    # Blood Raiders
    "corpii": ("blood", "frigate"),
    "dark corpii": ("blood", "frigate"),
    "elder corpii": ("blood", "frigate"),
    "corpior": ("blood", "destroyer"),
    "dark corpior": ("blood", "destroyer"),
    "corpum": ("blood", "cruiser"),
    "dark corpum": ("blood", "cruiser"),
    "elder corpum": ("blood", "cruiser"),
    "corpatis": ("blood", "battlecruiser"),
    "dark corpatis": ("blood", "battlecruiser"),
    "corpus": ("blood", "battleship"),
    "dark corpus": ("blood", "battleship"),
    "blood": ("blood", "any"),
    "dark blood": ("blood", "any"),
    "elder blood": ("blood", "any"),
    # Angel Cartel
    "gistii": ("angel", "frigate"),
    "arch gistii": ("angel", "frigate"),
    "gistior": ("angel", "destroyer"),
    "gistum": ("angel", "cruiser"),
    "arch gistum": ("angel", "cruiser"),
    "gistatis": ("angel", "battlecruiser"),
    "gist": ("angel", "battleship"),
    "angel": ("angel", "any"),
    "arch angel": ("angel", "any"),
    "domination": ("angel", "any"),
    # Serpentis
    "coreli": ("serpentis", "frigate"),
    "shadow coreli": ("serpentis", "frigate"),
    "corelior": ("serpentis", "destroyer"),
    "shadow corelior": ("serpentis", "destroyer"),
    "corelum": ("serpentis", "cruiser"),
    "shadow corelum": ("serpentis", "cruiser"),
    "corelatis": ("serpentis", "battlecruiser"),
    "shadow corelatis": ("serpentis", "battlecruiser"),
    "core": ("serpentis", "battleship"),
    "serpentis": ("serpentis", "any"),
    "shadow serpentis": ("serpentis", "any"),
    # Sansha's Nation
    "centii": ("sansha", "frigate"),
    "true centii": ("sansha", "frigate"),
    "centior": ("sansha", "destroyer"),
    "centum": ("sansha", "cruiser"),
    "true centum": ("sansha", "cruiser"),
    "centatis": ("sansha", "battlecruiser"),
    "centus": ("sansha", "battleship"),
    "true centus": ("sansha", "battleship"),
    "sansha": ("sansha", "any"),
    "sansha's": ("sansha", "any"),
    "true sansha": ("sansha", "any"),
    # Guristas Pirates
    "pithi": ("guristas", "frigate"),
    "dire pithi": ("guristas", "frigate"),
    "dread pithi": ("guristas", "frigate"),
    "pithior": ("guristas", "destroyer"),
    "pithum": ("guristas", "cruiser"),
    "dire pithum": ("guristas", "cruiser"),
    "pithatis": ("guristas", "battlecruiser"),
    "pith": ("guristas", "battleship"),
    "guristas": ("guristas", "any"),
    "dread guristas": ("guristas", "any"),
    # Amarr Empire NPC entities
    "imperial": ("amarr", "any"),
    "imperial templar": ("amarr", "any"),
    "divine": ("amarr", "any"),
    "divine imperial": ("amarr", "any"),
    "amarr navy": ("amarr", "any"),
    # Rogue Drones
    "strain": ("rogue_drone", "any"),
    "rogue": ("rogue_drone", "any"),
}

FAMILY_CLASS_PREFIXES: dict[tuple[str, str], list[str]] = {}
for _rp, (_fam, _cls) in RANK_PREFIX_META.items():
    FAMILY_CLASS_PREFIXES.setdefault((_fam, _cls), []).append(_rp)

ROGUE_DRONE_SUFFIXES_BY_CLASS: dict[str, tuple[str, ...]] = {
    "frigate": ("alvi",),
    "destroyer": ("alvior",),
    "cruiser": ("alvum", "alvu"),
    "battlecruiser": ("alvatis",),
    "battleship": ("alvus",),
}
ALL_ROGUE_DRONE_SUFFIXES: tuple[str, ...] = ("alvi", "alvior", "alvum", "alvu", "alvatis", "alvus")
ROGUE_DRONE_SUFFIX_TO_CLASS: dict[str, str] = {
    suf: cls for cls, sufs in ROGUE_DRONE_SUFFIXES_BY_CLASS.items() for suf in sufs
}

STRUCTURE_ALIASES: dict[str, list[str | tuple[str, ...]]] = {
    "stasis tower": ["{fac} Stasis Tower", "Stasis Webification Battery", "Pirate Stasis Tower"],
    "starbase stasis tower": ["{fac} Stasis Tower", "Stasis Webification Battery"],
    "bunker": ["{fac} Bunker", "Occupied {fac} Bunker"],
    "aux power array": ["Starbase Auxiliary Power Array", "Small Auxiliary Power Array", "Medium Auxiliary Power Array", "Large Auxiliary Power Array"],
    "auxiliary power array": ["Starbase Auxiliary Power Array", "Small Auxiliary Power Array", "Medium Auxiliary Power Array", "Large Auxiliary Power Array"],
    "habitat": ["Habitation Module"],
    "habitation module": ["Habitation Module"],
    "starbase control tower": ["{fac} Starbase Control Tower", "{fac} Control Tower"],
    "control tower": ["{fac} Control Tower", "{fac} Starbase Control Tower"],
    "point defense battery": ["{fac} Point Defense Battery", "Point Defense Battery"],
    "missile battery": ["{fac} Light Missile Battery", "{fac} Heavy Missile Battery", "{fac} Cruise Missile Battery"],
    "gistum missile battery": ["Angel Light Missile Battery", "Angel Heavy Missile Battery", "Angel Cruise Missile Battery"],
    "cruise missile battery": ["{fac} Cruise Missile Battery", "Cruise Missile Battery"],
    "heavy missile battery": ["{fac} Heavy Missile Battery", "Heavy Missile Battery"],
    "light missile battery": ["{fac} Light Missile Battery", "Light Missile Battery"],
    "torpedo battery": ["{fac} Torpedo Battery", "Torpedo Battery"],
    "energy neutralizer sentry": [
        ("{fac} Energy Neutralizer Sentry I", "{fac} Energy Neutralizer Sentry II", "{fac} Energy Neutralizer Sentry III"),
        "{fac} Energy Neutralizer Sentry",
    ],
    "tower sentry": [
        ("Tower Sentry {fac} I", "Tower Sentry {fac} II", "Tower Sentry {fac} III"),
        "{fac} Sentry Gun",
    ],
    "tower sentry i": ["Tower Sentry {fac} I"],
    "tower sentry ii": ["Tower Sentry {fac} II"],
    "tower sentry iii": ["Tower Sentry {fac} III"],
    "laser sentry": ["{fac} Laser Sentry", "Siege Beam Laser Sentry", "Siege Pulse Laser Sentry", "Ultra Fast Mobile Laser Sentry"],
    "light sentry": ["{fac} Light Missile Battery", "Tower Sentry {fac} I", "Light Missile Battery"],
    "sentry gun": ["{fac} Sentry Gun", "Tower Sentry {fac} I", "{fac} Heavy Missile Battery"],
    "silo": ["{fac} Silo", "Silo"],
    "warehouse": ["{fac} Warehouse", "Station Warehouse Container", "Warehouse"],
    "storage facility": ["Starbase Storage Facility", "{fac} Storage Facility", "Storage Facility"],
    "shipyard": ["Shipyard", "{fac} Shipyard", "{fac} Naval Shipyard"],
    "abandoned shipyard": ["Shipyard", "Dirty Shipyard"],
    "small shipyard": ["Shipyard"],
    "small refinery": ["Refinery", "Ore Refinery"],
    "prison": ["{fac} Prison", "Prison Facility", "Habitation Module - Prison"],
    "casino": ["{fac} Casino", "Habitation Module - Casino"],
    "smuggler stargate": ["Smuggler Stargate", "Pirate Gate"],
    "repair station": ["{fac} Repair Station", "Repair Station"],
    "repair outpost": ["{fac} Deadspace Repair Outpost", "{fac} Repair Outpost", "Repair Outpost"],
    "power generator": ["{fac} Power Generator", "Power Generator"],
    "shield generator": ["Starbase Shield Generator", "Fortified Starbase Shield Generator"],
    "reactor array": ["Starbase Reactor Array", "Complex Reactor Array", "Simple Reactor Array"],
    "drone sentry": ["Inactive Drone Sentry", "Rogue Drone Sentry"],
    "major assembly array": ["Starbase Major Assembly Array"],
    "low-tech energy harvester": ["Low-Tech Deadspace Energy Harvester"],
    "raider outpost": ["Blood Raider Logistics Outpost", "Blood Raider Military Outpost"],
    "gate sentry": ["{fac} Sentry Gun", "Tower Sentry {fac} I", "Tower Sentry {fac} II", "Tower Sentry {fac} III"],
    "station sentry": ["{fac} Sentry Gun", "Tower Sentry {fac} I", "Tower Sentry {fac} II", "Tower Sentry {fac} III"],
    "lookout tower": ["{fac} Lookout Tower", "Lookout Tower", "Observation Post"],
    "mind control tower": ["{fac} Starbase Control Tower", "{fac} Control Tower"],
    "recreational supplies": ["Exploration Supplies", "Expedition Secure Supplies"],
    "abandoned ship yard": ["{fac} Component Deposit Yard", "Shipyard", "Dirty Shipyard"],
    "hackable container": ["Cargo Container"],
    "structure container": ["Cargo Container"],
    "floating container": ["Cargo Container"],
    "container": ["Cargo Container"],
    "private hangar": ["Hangar", "Station Warehouse Container"],
    "outpost": ["{fac} Outpost", "Outpost"],
    "sansha outpost": ["Sansha Control Tower", "Sansha Station"],
    "syndicate cruise missile battery": ["Cruise Missile Battery"],
    "syndicate heavy missile battery": ["Heavy Missile Battery"],
    "khanid point defense battery": ["Point Defense Battery", "Amarr Point Defense Battery"],
    "prison cruise missile battery": ["Cruise Missile Battery"],
}

BARE_FAMILY_PREFIXES = {
    "coreli", "corelior", "corelum", "corelatis", "core",
    "corpii", "corpior", "corpum", "corpatis", "corpus",
    "centii", "centior", "centum", "centatis", "centus",
    "gistii", "gistior", "gistum", "gistatis", "gist",
    "pithi", "pithior", "pithum", "pithatis", "pith",
    "shadow serpentis", "dread guristas", "dark blood", "true sansha", "domination",
    "corelum guardian chief", "shadow corelum chief", "guardian angel", "guardian angels",
    "federation prak", "federation praktor", "maru",
}

ORE_STRIP_RE = re.compile(
    r"^(Plagioclase|Veldspar|Scordite|Pyroxeres|Kernite|Omber|Jaspet|Hemorphite|Hedbergite|"
    r"Gneiss|Dark Ochre|Crokite|Spodumain|Bistot|Arkonor|Mercoxit|Ice|Bane|Banidine|Augumene|"
    r"Mercium|Lyavite|Pithix|Green Arisite|Geodite|Polygypsum|Oeryl|Azure Ice|Crystalline Icicles|"
    r"Glacial Mass|Clear Icicle|White Glaze|Blue Ice|Dark Glitter|Glare Crust|Gelidus|Krystallos)\b",
    re.I,
)

PART_QUALIFIER_RE = re.compile(r"^\d+\s+of\s+\d+$", re.I)
TRAILING_PARENS_RE = re.compile(r"\s*\(([^()]*)\)\s*$")
HEADING_RE = re.compile(r"^(={2,6})\s*(.*?)\s*\1\s*$", re.M)

ROOM_HEADING_RE = re.compile(
    r"\b(?:pocket|room|stage|chamber|hall|area|zone|warp-?in)\b|"
    r"\b(?:first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th|initial|final|last|gated)\s+(?:pocket|room|area|stage)\b",
    re.I,
)

STAGE_HEADING_RE = re.compile(
    r"^(?:"
    r"part\s*#?\d+(?:\s+of\s+\d+)?(?:\s*[-:].*)?|"
    r"location\s+#?(?:\d+|one|two|three|four|five)(?:\s*\([^)]*\))?|"
    r"(?:first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th)(?:\s+and\s+last)?\s+location(?:\s*\([^)]*\))?"
    r")$",
    re.I,
)

VARIANT_HEADING_RE = re.compile(
    r"\b(?:variant|variation(?:\s+\d+)?|type\s*\d+)\b|"
    r"\blayout\b|"
    r"\bwalkthrough\s*\([^)]+\)|"
    r"\b(?:ring|station|pirate\s+gate|chem(?:ical)?\s+factory|asteroid(?:\s+port)?|mining\s+port|rock(?:\s+haven)?|gas(?:\s+haven)?)\s+spawn\b",
    re.I,
)

NON_VARIANT_SECTION_RE = re.compile(
    r"\b(?:escalation|rewards?|loot|bounty|see\s+also|external\s+links?|notes?|tips?|references?|mining|salvage|trigger)\b",
    re.I,
)

NPC_ROLE_HINT_RE = re.compile(
    r"\b(?:outlaw|bandit|gunslinger|desperado|marauder|raider|ambusher|defender|protector|"
    r"guard|bodyguard|gatekeeper|mercenary|scout|watchman|fighter|sniper|officer|commander|"
    r"escort|pirate|brigand|spy|sentry|frigate|destroyer|cruiser|battlecruiser|battleship|"
    r"transport|hauler|enforcer|overseer|supervisor|envoy|veteran|recruit|hacker|swarmer|"
    r"thief|punk|assassin|infiltrator|saboteur|warden|grunt|captor|smuggler|"
    r"lieutenant|captain|admiral|general|chief|lord|boss|drone|wingman|escapee|terrorist)\b",
    re.I,
)

CLASSIFICATION_STATES = ("explicit", "inferred", "unclassified")

TRIGGER_ENUM = (
    "this_ship_destroyed",
    "last_in_group_destroyed",
    "last_of_class_destroyed",
    "ship_approached",
    "timed",
    "objective_complete",
    "group_destroyed",
)

OBJECTIVE_KIND_ENUM = (
    "kill_all",
    "kill_target",
    "destroy_structure",
    "retrieve_item",
    "deliver_item",
    "mine_ore",
    "approach",
    "hack",
    "manufacture",
    "talk_to_agent",
    "branch_choice",
    "travel",
)


def default_wiki_dir() -> Path:
    env = os.environ.get("EVE_WIKI_DIR")
    if env:
        return Path(env).expanduser()
    return Path.home() / "ai" / "eve-wiki"


def default_sde_db(repo_root: Path = REPO_ROOT) -> Path:
    env = os.environ.get("EVOMU_SDE_PATH")
    if env:
        return Path(env).expanduser()
    local = repo_root / "data" / "evomu_sde.sqlite"
    if local.exists():
        return local
    if repo_root.parent.name == ".worktrees":
        shared = repo_root.parent.parent / "data" / "evomu_sde.sqlite"
        if shared.exists():
            return shared
    return local


def default_mission_ext(repo_root: Path = REPO_ROOT) -> Path:
    local = repo_root / "data" / "sde_agent_mission_ext.json"
    if local.exists():
        return local
    if repo_root.parent.name == ".worktrees":
        shared = repo_root.parent.parent / "data" / "sde_agent_mission_ext.json"
        if shared.exists():
            return shared
    return local


def default_overrides_path(repo_root: Path = REPO_ROOT) -> Path:
    """Return default path to tracked structured wiki PvE overrides JSON."""
    return repo_root / "tools" / "wiki_pve_overrides.json"


def default_coverage_ratchet_path(repo_root: Path = REPO_ROOT) -> Path:
    """Return default path to tracked wiki PvE coverage ratchet JSON."""
    return repo_root / "tools" / "wiki_pve_coverage_ratchet.json"


def clean_wiki_inline(text: str) -> str:
    """Clean MediaWiki inline markup into plain readable text."""
    s = html.unescape(text)
    s = re.sub(r"<ref[^>]*>.*?</ref>", "", s, flags=re.S | re.I)
    s = re.sub(r"<ref[^/]*/>", "", s, flags=re.I)
    s = re.sub(r"\{\{\s*m3\s*\}\}", " m3 ", s, flags=re.I)
    s = re.sub(r"<sup[^>]*>3</sup>", "3", s, flags=re.I)
    s = re.sub(r"<br\s*/?>", " ", s, flags=re.I)
    s = re.sub(r"<[^>]+>", " ", s)
    s = re.sub(r"\[\[File:[^\]]+\]\]", "", s, flags=re.I)
    s = re.sub(r"\[\[([^|\]]+\|)?([^\]]+)\]\]", r"\2", s)
    s = re.sub(r"\{\{[^{}]*\}\}", "", s)
    s = re.sub(r"''+", "", s)
    s = (
        s.replace("\u2013", "-")
        .replace("\u2014", "-")
        .replace("\xa0", " ")
        .replace("…", "...")
        .replace("m³", " m3 ")
    )
    s = re.sub(r"\s+", " ", s).strip()
    if len(s) >= 2 and ((s[0] == "'" and s[-1] == "'") or (s[0] == '"' and s[-1] == '"')):
        s = s[1:-1].strip()
    return s


def infer_factions_from_text(text: str) -> list[str]:
    """Extract all canonical faction names matched in text in priority order."""
    low = text.strip()
    if not low:
        return []
    found: list[str] = []
    for pat, canon in _FACTION_PATTERNS:
        if pat.search(low) and canon not in found:
            found.append(canon)
    return found


def infer_faction_from_text(text: str) -> str | None:
    """Extract the highest-priority canonical faction name from text."""
    factions = infer_factions_from_text(text)
    return factions[0] if factions else None


def norm_title(title: str) -> str:
    s = html.unescape(title)
    s = s.replace("\u2013", "-").replace("\u2014", "-").replace("’", "'").replace("`", "'")
    return re.sub(r"\s+", " ", s).strip()


def parse_title_qualifiers(title: str) -> dict[str, Any]:
    """Strip trailing bracketed level/faction/type qualifiers while preserving mission part numbers."""
    base = norm_title(title)
    qualifiers: list[str] = []
    levels: list[int] = []
    factions: list[str] = []
    while True:
        m = TRAILING_PARENS_RE.search(base)
        if not m:
            break
        inside = m.group(1).strip()
        if PART_QUALIFIER_RE.match(inside):
            break
        qualifiers.append(inside)
        base = base[: m.start()].strip()
    qualifiers.reverse()

    part_match = re.search(r"\((\d+)\s+of\s+(\d+)\)\s*$", base, re.I)
    part_info = (
        {"part": int(part_match.group(1)), "of": int(part_match.group(2))}
        if part_match
        else None
    )

    for q in qualifiers:
        m_lvl = re.search(r"\blevel\s*(\d+)\b|\bL(\d+)\b", q, re.I)
        if m_lvl:
            lvl = int(m_lvl.group(1) or m_lvl.group(2))
            if lvl not in levels:
                levels.append(lvl)
        for fac in infer_factions_from_text(q):
            if fac not in factions:
                factions.append(fac)
        if re.fullmatch(r"ded", q.strip(), re.I) and "CONCORD Assembly" not in factions:
            factions.append("CONCORD Assembly")

    return {
        "base_title": base,
        "qualifiers": qualifiers,
        "levels": levels,
        "factions": factions,
        "part_info": part_info,
    }


def make_title_lookup_keys(name: str, strict_only: bool = False) -> list[str]:
    """Generate deterministic lookup keys for matching wiki names against SDE missions/dungeons."""
    t = norm_title(name)
    if not t:
        return []
    variants = [t]
    if not strict_only:
        tl = t.lower()
        if tl.startswith("blood ") and not tl.startswith("blood raider"):
            variants.append("Blood Raider " + t[6:])
        if tl.startswith("amarr "):
            variants.append("Amarrian " + t[6:])
        if "smonsequences" in tl:
            variants.append(re.sub(r"smonsequences", "Schmonsequences!", t, flags=re.I))
        m_part = re.match(r"^(.*?),\s*(\d+)(?:st|nd|rd|th)\s+Part$", t, re.I)
        if m_part:
            variants.append(f"{m_part.group(1)} P{m_part.group(2)}")
            variants.append(f"{m_part.group(1)} {m_part.group(2)}")
        if " - " in t and not STAGE_HEADING_RE.match(t):
            variants.append(t.split(" - ", 1)[1].strip())
        if tl.startswith("contested "):
            # Strip empire qualifier e.g. "Contested Gallente Skeleton Comet" -> "Contested Skeleton Comet"
            for emp in ("gallente ", "caldari ", "amarr ", "minmatar "):
                if tl.startswith("contested " + emp):
                    variants.append("Contested " + t[len("contested " + emp) :])

    out: list[str] = []
    seen: set[str] = set()
    for v in variants:
        for k in (
            v.lower(),
            re.sub(r"[^\w\s-]", "", v).strip().lower(),
            re.sub(r"[^a-z0-9]+", " ", v.lower()).strip(),
        ):
            if k and k not in seen:
                seen.add(k)
                out.append(k)
    return out


def parse_count(raw: str) -> dict[str, Any]:
    """Parse NPCTableRow count string into structured {min, max, raw}."""
    s = clean_wiki_inline(raw)
    if not s:
        return {"min": None, "max": None, "raw": ""}
    if s in ("?", "??", "unknown"):
        return {"min": None, "max": None, "raw": s}
    m_rng = re.match(r"^~?\s*(\d+)\s*(?:-|to|/)\s*(\d+|\?)$", s, re.I)
    if m_rng:
        lo = int(m_rng.group(1))
        hi = None if m_rng.group(2) == "?" else int(m_rng.group(2))
        if hi is not None and hi < lo:
            lo, hi = hi, lo
        return {"min": lo, "max": hi, "raw": s}
    m_plus = re.match(r"^~?\s*(\d+)\s*\+$", s)
    if m_plus:
        return {"min": int(m_plus.group(1)), "max": None, "raw": s}
    m_single = re.match(r"^~?\s*(\d+)\s*x?\??$", s, re.I)
    if m_single:
        v = int(m_single.group(1))
        return {"min": v, "max": v, "raw": s}
    m_any = re.search(r"(\d+)\s*-\s*(\d+)", s)
    if m_any:
        lo, hi = int(m_any.group(1)), int(m_any.group(2))
        if hi < lo:
            lo, hi = hi, lo
        return {"min": lo, "max": hi, "raw": s}
    m_first = re.search(r"(\d+)", s)
    if m_first:
        v = int(m_first.group(1))
        return {"min": v, "max": v, "raw": s}
    return {"min": None, "max": None, "raw": s}


def parse_point(raw: str) -> str | None:
    s = clean_wiki_inline(raw).strip(" )].").lower()
    if not s or s in ("no", "none", "false", "-"):
        return None
    if "scram" in s:
        return "warp_scramble"
    if s in ("yes", "true", "point", "disrupt", "warp disrupt", "1") or "point" in s or "disrupt" in s:
        return "warp_disrupt"
    return None


def parse_ewar(ewar_values: list[str], point_val: str | None) -> dict[str, Any]:
    """Normalize EWAR codes, icon templates, File icons, and point/scram status."""
    tags: set[str] = set()
    raw_cleaned: list[str] = []
    if point_val:
        tags.add(point_val)

    token_map = (
        ("web", re.compile(r"\bweb(?:ifier|s|ification)?\b|stasis\s+webifier", re.I)),
        ("neut", re.compile(r"\bneut(?:ing|ralizer|s)?\b|energy\s+neutraliz", re.I)),
        ("nos", re.compile(r"\bnos(?:feratu)?\b", re.I)),
        ("td", re.compile(r"\btd\b|tracking\s+disrupt", re.I)),
        ("gd", re.compile(r"\bgd\b|guidance\s+disrupt", re.I)),
        ("damp", re.compile(r"\bdamp(?:ening|ener|s)?\b|sensor\s+damp", re.I)),
        ("tp", re.compile(r"\btp\b|painter|target\s+paint", re.I)),
        ("ecm", re.compile(r"\becm\b|\bjam(?:ming|mer|s)?\b", re.I)),
        ("rr", re.compile(r"\brr\b|remote\s+(?:armor\s+|shield\s+)?repair", re.I)),
        ("warp_scramble", re.compile(r"\bscram(?:ble|bler)?\b", re.I)),
        ("warp_disrupt", re.compile(r"warp\s+disrupt|\bpoint\b", re.I)),
    )

    for raw in ewar_values:
        if not raw or not raw.strip():
            continue
        # Extract {{icon|CODE|...}} and [[File:...]] hints before stripping templates
        extra_bits = list(re.findall(r"\{\{\s*icon\s*\|\s*([^|}]+)(?:\|[^|}]*)?(?:\|([^}]+))?\}\}", raw, re.I))
        icon_hint = " ".join(f"{a} {b}" for a, b in extra_bits)
        file_hints = " ".join(re.findall(r"\[\[File:([^\]]+)\]\]", raw, re.I))
        plain = clean_wiki_inline(raw)
        if plain:
            raw_cleaned.append(plain)
        combined = " ".join(x for x in (plain, icon_hint, file_hints) if x)
        for tag, pat in token_map:
            if pat.search(combined):
                tags.add(tag)

    return {
        "tags": sorted(tags),
        "raw": raw_cleaned,
    }


_DAMAGE_CONDITION_RE = re.compile(
    r"\bdama?g(?:e[sd]?|ing)\b|"
    r"\b\d+\s*%\s*(?:hp|armor|hull|shields?|structure)\b|"
    r"\b(?:armor|hull|shields?|structure)\s*(?:damage|damaged|breach|depleted|low|gone|broken|hits?|threshold|(?:reaches?|drops?\s+(?:to|below|under)|falls?\s+(?:to|below|under)|at|below|under)\s+\d+)\b|"
    r"\b(?:damag(?:e|ed|es|ing)|hit(?:s|ting)?|break(?:s|ing)?)\s+(?:\w+\s+){0,2}(?:shields?|armor|hull|structure)\b|"
    r"%\s*(?:hp|armor|hull|shields?|structure)\b",
    re.I,
)


_UNCERTAIN_TRIGGER_RE = re.compile(
    r"(?:\?\s*$|\b(?:possible|possibly|unconfirmed|not\s+confirmed|may\s+(?:or\s+may\s+not\s+)?trigger)\b)",
    re.I,
)


def classify_trigger(text: str) -> dict[str, Any] | None:
    """Classify a free-form wiki trigger string into 3-state trigger schema (explicit or unclassified)."""
    cleaned = clean_wiki_inline(text)
    if not cleaned:
        return None
    low = cleaned.lower()

    if _UNCERTAIN_TRIGGER_RE.search(cleaned):
        return {
            "kind": None,
            "classification_state": "unclassified",
            "inference_rule": None,
            "original_text": cleaned,
            "condition": None,
        }

    condition = "on_destroy"
    if re.search(
        r"\battack(?:ed|ing|ign|s)?\b|\baggress(?:ed|ing)?\b|\bagress(?:ed|ing)?\b|\bshoot(?:ing)?\b|\bharm(?:ed|ing)?\b|\bhair[-\s]?trigger\b",
        low,
    ):
        condition = "on_attack"
    elif _DAMAGE_CONDITION_RE.search(low) or re.search(r"\b(?:low|at|on)\s+(?:shields?|armor|hull|structure)\b", low):
        condition = "on_damage"

    unlocks_gate = bool(re.search(r"\bunlock(?:s|ing)?\s+(?:the\s+)?(?:acceleration\s+)?gate\b|\bgate\s+unlock\b", low))
    is_escalation = bool(re.search(r"\bescall?ation\b|\bexpedition\b", low))

    m_sec = re.search(r"\b(\d+)\s*(?:s|sec|seconds?)\b", low)
    m_min = re.search(r"\b(\d+)\s*(?:min|mins|minutes?)\b", low)
    m_hr = re.search(r"\b(\d+)\s*(?:h|hr|hrs|hours?)\b", low)
    time_seconds: int | None = None
    if m_sec:
        time_seconds = int(m_sec.group(1))
    elif m_min:
        time_seconds = int(m_min.group(1)) * 60
    elif m_hr:
        time_seconds = int(m_hr.group(1)) * 3600

    generic_last = {
        "one", "ship", "ships", "npc", "npcs", "enemy", "enemies", "hostile", "hostiles",
        "target", "targets", "unit", "units", "group", "wave", "spawn", "fleet", "wing", "pack",
        "pocket", "room", "remaining", "surviving", "alive", "left", "standing",
        "in", "of", "the", "a", "an", "this", "that", "to", "be",
        "triggers", "trigger", "spawns", "spawn", "killed", "destroyed", "dead",
    }

    def _matches_named_last_target() -> bool:
        for pat in (
            r"\b(?:destruction|killing|death)\s+of\s+(?:the\s+)?last\s+([a-z0-9' \-]+?)(?:\s+(?:is\s+)?(?:triggers?|spawns?|will|causes?|in\b|to\b)|$|[.,;])",
            r"\blast\s+([a-z0-9' \-]+?)\s+(?:is\s+)?(?:destroyed|killed|dead|triggers?|spawns?|to\s+(?:die|spawn|be\s+(?:killed|destroyed)))",
            r"^last\s+([a-z0-9' \-]+)$",
        ):
            for m in re.finditer(pat, low):
                phrase = m.group(1).strip()
                if re.search(r"\b(?:in|of)\s+(?:the\s+)?(?:group|wave|spawn|fleet|wing|pack)\b", phrase):
                    continue
                toks = [t for t in phrase.split() if t not in {"the", "a", "an", "remaining", "surviving"}]
                if toks and not all(t in generic_last for t in toks):
                    return True
        return False

    # Priority order from most specific to most general; unrecognized triggers stay unclassified
    if (
        re.search(
            r"\b(?:last|final)\b.*\b(?:of|ship\s+of|in)\b.*\b"
            r"(?:class|frigate|destroyer|cruiser|battlecruiser|battleship|industrial|drone)\b",
            low,
        )
        or re.search(r"\blast\s+(?:of|kill\s+of|ship\s+(?:killed\s+)?of)\s+(?:this|the|that)?\s*class\b", low)
        or re.search(r"\bdestroying\s+all\s+(?:the\s+)?(?:frigates|destroyers|cruisers|battlecruisers|battleships)\b", low)
        or re.search(r"\blast\s+lord\s+admiral\b", low)
        or _matches_named_last_target()
    ):
        kind: str | None = "last_of_class_destroyed"
    elif (
        re.search(r"\b(?:last|final)\b.*\b(?:in|of)\b.*\b(?:group|wave|spawn|fleet|wing|pack)\b", low)
        or re.search(
            r"\b(?:last|final)\b(?:\s+one|\s+ship|\s+npc|\s+kill)?\s*(?:is\s+)?(?:killed|destroyed|dead|left|triggers?|trigger\b|\bto\s+(?:die|spawn))\b",
            low,
        )
        or re.search(r"\bfinishing\s+wave\b", low)
        or re.search(r"^(?:last|lastship)\b", low)
        or re.search(r"\ball\b.*\b(?:destroyed|killed|dead)\b", low)
    ):
        kind = "last_in_group_destroyed"
    elif re.search(
        r"\b(?:approach(?:ed|ing)?|within\s+\d+\s*(?:k?m)|proximity|warping\s+in|on\s+warp|range|distance|too\s+close|orbit|get\s+within|enter(?:s|ing)?)\b",
        low,
    ):
        kind = "ship_approached"
    elif (
        time_seconds is not None
        or re.search(
            r"\b(?:timer|timed|delay|after\s+\d+\s*(?:sec|min|hr|s\b)|every\s+\d+|over\s+time|"
            r"after\s+(?:a\s+)?(?:couple\s+of\s+|few\s+|several\s+)?(?:seconds?|minutes?|mins?|hours?)|"
            r"arrives\s+several\s+minutes?|after\s+a\s+short\s+while\s+after)\b",
            low,
        )
    ):
        kind = "timed"
    elif (
        unlocks_gate
        or re.search(
            r"\b(?:mission\s+objective|mission\s+complete|mission\s+completion|mission\s+targets?|mission\s+objects?|"
            r"complete\s+(?:the\s+)?mission|completes?\s+(?:the\s+)?(?:mission|objectives?)|\bobjective\b|"
            r"ends?\s+(?:the\s+)?(?:expedition|mission|site)|expedition\s+end|kill\s+to\s+complete|"
            r"destroy\s+to\s+complete|needs?\s+to\s+be\s+killed|hack(?:ed|ing)?|loot(?:ed|ing)?|drop(?:s|ped)?\b.*\bobjective)\b",
            low,
        )
    ):
        kind = "objective_complete"
    elif (
        _DAMAGE_CONDITION_RE.search(low)
        or re.search(r"\b(?:low|at|on)\s+(?:shields?|armor|hull|structure)\b", low)
        or re.search(
            r"\b(?:hair[-\s]?trigger|attack(?:ed|ing|ign|s)?|shoot(?:ing)?|aggress(?:ed|ing)?|agress(?:ed|ing)?|harm(?:ed|ing)?|"
            r"escall?ations?|triggers?\s+(?:for\s+)?(?:(?:(?!\?\b|possible\b|unconfirmed\b|if\b|prevents?\b)[a-z0-9#-]+)\s+){0,4}"
            r"(?:waves?|reinforcements?|spawns?|groups?|fleets?|frigates?|destroyers?|cruisers?|battlecruisers?|battleships?|neuting\b|chain\b)|"
            r"spawns?\s+(?:next|waves?|reinforcements?|defenders?|group|two|more)|"
            r"killing\s+spawns?|\b(?:destroy|kill)\b.*\bto\s+(?:spawn|trigger)\b|destroying|destroyed|killed|destruction|upon\s+death|on\s+(?:death|destruction)|"
            r"highest\s+bounty|^yes$|^trigger$|^waves?\b|^next\s+wave|^reinforcements?|chain\s+trigger)\b",
            low,
        )
    ):
        kind = "this_ship_destroyed"
    else:
        return {
            "kind": None,
            "classification_state": "unclassified",
            "inference_rule": None,
            "original_text": cleaned,
            "condition": None,
        }

    assert kind in TRIGGER_ENUM
    res: dict[str, Any] = {
        "kind": kind,
        "classification_state": "explicit",
        "inference_rule": None,
        "original_text": cleaned,
        "condition": condition,
    }
    if time_seconds is not None:
        res["time_seconds"] = time_seconds
    if unlocks_gate:
        res["unlocks_gate"] = True
    if is_escalation:
        res["is_escalation"] = True
    return res


_HDR_LEADING_DIST_RE = re.compile(
    r"^\s*(?:~?\d[\d.,\s–-]*(?:km|kilometers?|kilometres?|meters?|metres?|m|au)(?![a-z0-9])\s*(?:[,;-]\s*|\band\b\s*|\s+))+",
    re.I,
)
_HDR_PURE_AGGRO_PAREN_RE = re.compile(
    r"^(?:(?:no(?:[-\s]+auto)?|auto(?:matic)?|delayed|instant)\s*[-\s]?aggro\b.*"
    r"|aggro\s+(?:on|at|upon)\s+(?:arrival|warp[-\s]?in|grid)\.?"
    r"|(?:on|at|upon)\s+(?:arrival|warp[-\s]?in|grid)\.?)$",
    re.I,
)
_HDR_AGGRO_GUARD_RE = re.compile(
    r"\b(?:spawns?|waves?|reinforcements?|triggers?|triggered|destroy(?:ed|ing)?|kill(?:ed|ing)?|"
    r"attack(?:ed|ing)?|aggress(?:ed|ing|ion)?|shoot(?:ing)?|hack(?:ed|ing)?|"
    r"after\s+\d+|every\s+\d+|shields?|armor|hull|structure)\b",
    re.I,
)
_HDR_NON_TRIGGER_NOTE_RE = re.compile(
    r"^(?:triggers?\s*:.*|triggers?\b(?!ed)\b.*|hair[-\s]?trigger\b.*|"
    r"(?:high|low|null)[-\s]?sec\b.*|"
    r"spawns?\s+(?:(?:the|a|an|next|more|additional)\s+){0,2}(?:waves?|reinforcements?|groups?|as\s+(?:\d+\s+|a\s+single\s+)?groups?)\b.*)$",
    re.I,
)
_HDR_PREVENTIVE_OR_DIST_RE = re.compile(
    r"(?:\bprevents?\s+(?:this\s+|the\s+)?spawn\b|\blast\s+kill\s+triggers?\s+(?:the\s+)?next\b|"
    r"\btriggers?\s+(?:the\s+|a\s+|an\s+|next\s+|additional\s+|more\s+)?(?:wave|reinforcements?|group|spawn)\b|"
    r"^\s*(?:from|off|at|near)\s+(?:trigger|warp[-\s]?in|beacon|gate)\b)",
    re.I,
)
_HDR_INCOMING_HINT_RE = re.compile(
    r"\b(?:destroy(?:ed|ing)?|kill(?:ed|ing)?|last|approach(?:ed|ing)?|"
    r"min|mins|minutes?|sec|secs|seconds?|hr|hrs|hours?|"
    r"timer|timed|when|"
    r"on\s+(?:destroy(?:ed|ing)?|kill(?:ed|ing)?|attack(?:ed|ing)?|aggress(?:ed|ing|ion)?|shoot(?:ing)?|hack(?:ed|ing)?|damag?e|death|destruction|approach(?:ed|ing)?|proximity|activat(?:ing|ion)|trigger(?:ed|ing)?|last\b|\d+\s*%)|"
    r"upon|after|"
    r"attack(?:ed|ing)?|aggress(?:ed|ing)?|shoot(?:ing)?|hack(?:ed|ing)?|"
    r"triggers?|triggered|"
    r"\d+\s*%\s*(?:shields?|armor|hull|structure)|"
    r"(?:shields?|armor|hull|structure)\s*(?:damage|damaged|breach|depleted|low|gone|broken|hits?|threshold|(?:reaches?|at|below|under)\s+\d+)|"
    r"(?:damag(?:e|ed|ing)|hit(?:s|ting)?|break(?:s|ing)?)\s+(?:\w+\s+){0,2}(?:shields?|armor|hull|structure))\b",
    re.I,
)


def _is_non_trigger_header_clause(text: str) -> bool:
    """Return True if a header clause encodes outgoing triggers, spatial anchors, sec-zone notes, or pure aggro/arrival notes."""
    no_dist = _HDR_LEADING_DIST_RE.sub("", text).strip()
    if not no_dist:
        return True
    if _HDR_NON_TRIGGER_NOTE_RE.match(no_dist) and not re.match(
        r"^triggers?\s*:.*(?:destroyed|killed|attacked|damaged|approached)\b", no_dist, re.I
    ):
        return True
    if not re.search(r"\btriggered\b", no_dist, re.I) and _HDR_PREVENTIVE_OR_DIST_RE.search(no_dist):
        return True
    return bool(_HDR_PURE_AGGRO_PAREN_RE.match(no_dist) and not _HDR_AGGRO_GUARD_RE.search(no_dist))


def _extract_header_trigger_probe(cleaned: str) -> str | None:
    """Extract the incoming-trigger clause from an NPCTableHead/Separator label, skipping outgoing/spatial/aggro parens."""
    for m_paren in re.finditer(r"\(([^()]+)\)", cleaned):
        inner = m_paren.group(1).strip()
        kept_clauses = [
            seg.strip()
            for seg in re.split(r"[,;]+", inner)
            if seg.strip() and not _is_non_trigger_header_clause(seg.strip())
        ]
        if not kept_clauses:
            continue
        inner_eff = ", ".join(kept_clauses)
        if _is_non_trigger_header_clause(inner_eff):
            continue
        if _HDR_INCOMING_HINT_RE.search(inner_eff):
            return inner_eff

    unparen = re.sub(r"\s{2,}", " ", re.sub(r"\([^)]*\)", " ", cleaned)).strip(" -–:;,")
    low_unparen = unparen.lower()
    if unparen and re.search(
        r"\b(?:triggered\s+by|on\s+destroying|upon\s+destroying|after\s+\d+\s*(?:mins?|minutes?|secs?|seconds?|hrs?|hours?)|"
        r"\d+\s*(?:mins?|minutes?|secs?|seconds?|hrs?|hours?)\s+(?:after|from|past)\b|"
        r"within\s+\d+\s*(?:k?m)|last\s+(?:ship|in|of)|"
        r"spawns?\s+(?:after|in\s+\d+\s*(?:mins?|minutes?|secs?|seconds?|hrs?|hours?)|when|if|on|upon)|"
        r"\d+\s*%\s*(?:shields?|armor|hull|structure))\b",
        low_unparen,
    ):
        return re.sub(
            r"^(?:(?:initial\s+)?(?:wave|group|spawn|phase)\s*(?:#?\d+|[ivx]+)?|(?:timed\s+)?reinforcements?)\s*[-–:]\s*",
            "",
            unparen,
            flags=re.I,
        ).strip() or unparen
    return None


def classify_wave_header_trigger(header: str) -> dict[str, Any] | None:
    """Extract trigger metadata when NPCTableHead/Separator encodes when a wave spawns."""
    cleaned = clean_wiki_inline(header)
    if not cleaned:
        return None
    probe = _extract_header_trigger_probe(cleaned)
    if not probe:
        return None
    res = classify_trigger(probe)
    if res:
        res["from_wave_header"] = cleaned
    return res


def _parse_number_token(tok: str) -> float | None:
    s = tok.strip()
    if not s:
        return None
    if re.match(r"^\d+,\d$", s):
        return float(s.replace(",", "."))
    try:
        return float(s.replace(",", ""))
    except ValueError:
        return None


_WAVE_DISTANCE_RE = re.compile(
    r"(?:~|approx\.?\s*)?(\d[\d,]*(?:\.\d+)?)\s*"
    r"(?:(km|meters?|metres?|m|au)\s*)?"
    r"(?:[-–]\s*(\d[\d,]*(?:\.\d+)?)\s*)?"
    r"(km|meters?|metres?|m|au)(?![a-z0-9])",
    re.I,
)

_STRUCT_DIST_PREFIX_RE = re.compile(
    r"\b(?:wave|group|spawn|reinforcements?|pocket|room|stage|phase|part|tier|level|fleet|batter(?:y|ies))\s*#?\s*$",
    re.I,
)

_UNPARSED_DISTANCE_PATTERNS = (
    re.compile(
        r"(?:(?:<=?|>=?|±)\s*\d[\d,]*(?:\.\d+)?|\d[\d,]*(?:\.\d+)?\s*\+|"
        r"\d[\d,]*(?:\.\d+)?(?:\s*(?:km|kilometers?|kilometres?|meters?|metres?|m|au))?\s*(?:/|±|\bor\b|\bto\b|\.\.)\s*\d[\d,]*(?:\.\d+)?|"
        r"\bbetween\s+\d[\d,]*(?:\.\d+)?(?:\s*(?:km|kilometers?|kilometres?|meters?|metres?|m|au))?\s+and\s+\d[\d,]*(?:\.\d+)?|"
        r"\b(?:over|under|beyond|more\s+than|less\s+than)\s+\d[\d,]*(?:\.\d+)?|\?+)"
        r"\s*(?:km|kilometers?|kilometres?|meters?|metres?|au)(?![a-z0-9])(?:\s*\+)?|"
        r"\b\d[\d,]*(?:\.\d+)?\s*(?:km|kilometers?|kilometres?|meters?|metres?|au)\s*\+",
        re.I,
    ),
    re.compile(
        r"(?:within|range|distance|orbit|proximity|away|from|off)\s+(?:<=?|>=?|±|\+)\s*\d[\d,]*(?:\.\d+)?\s*m(?![a-z0-9])|"
        r"(?:(?:<=?|>=?|±)\s*(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?|\b(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?\s*\+)\s*m(?![a-z0-9])|"
        r"\b(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?\s*m\s*\+|"
        r"\b(?:over|under|beyond|more\s+than|less\s+than)\s+(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?\s*m(?![a-z0-9])|"
        r"\b(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?(?:\s*m)?\s*(?:/|±|\bor\b|\bto\b|\.\.)\s*(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?\s*m(?![a-z0-9])|"
        r"\bbetween\s+(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?(?:\s*m)?\s+and\s+(?:\d[\d,]*,\d{3}|\d{3,})(?:\.\d+)?\s*m(?![a-z0-9])",
        re.I,
    ),
    re.compile(
        r"\b\d[\d,]*(?:\.\d+)?\s*k\s+(?:from|away|off|out)\b|"
        r"\(\s*\d[\d,]*(?:\.\d+)?(?:\s*[-–]\s*\d[\d,]*(?:\.\d+)?)?\s*k\s*\)",
        re.I,
    ),
    re.compile(r"\(\s*(?:[a-z\s]*\b)?(?:km|kilometers?|kilometres?|meters?|metres?|au)\s*\)", re.I),
    re.compile(r"\(\s*(\d+)\s*[-–]\s*(\d+)\s*\)"),
)


def parse_wave_distance(wave_name: str, group_name: str | None = None) -> dict[str, int | float] | None:
    """Parse unambiguous listed wave/group distance notation (e.g. '50-80km', '~5000m') into meters."""
    probes = [group_name, wave_name] if group_name else [wave_name]
    unit_scale = {"m": 1.0, "meter": 1.0, "meters": 1.0, "metre": 1.0, "metres": 1.0, "km": 1000.0, "au": 149_597_870_700.0}
    for probe in probes:
        if not probe:
            continue
        cleaned = clean_wiki_inline(probe)
        pos = 0
        while pos < len(cleaned):
            m = _WAVE_DISTANCE_RE.search(cleaned, pos)
            if not m:
                break
            v1 = _parse_number_token(m.group(1))
            unit1 = (m.group(2) or m.group(4)).lower()
            v2 = _parse_number_token(m.group(3) or "") if m.group(3) else None
            unit2 = m.group(4).lower()
            if v1 is None:
                pos = m.end()
                continue
            # Reject comparator/plus/slash/range-word bounds (e.g. '>50km', '20km+', '50km to 80km', 'between 20km and 40km')
            before = cleaned[: m.start()]
            after = cleaned[m.end() :]
            if (
                re.search(r"(?:<=?|>=?|±|[/?]|\.\.)\s*$", before)
                or re.search(
                    r"(?:\d[\d,]*(?:\.\d+)?\s*(?:km|kilometers?|kilometres?|meters?|metres?|m|au)?\s+(?:to|or|and)\s+|"
                    r"\b(?:between|over|under|beyond|more\s+than|less\s+than)\s+)$",
                    before,
                    re.I,
                )
                or re.match(
                    r"^\s*(?:\+|±|\.\.|/\s*(?:\d|\?)|"
                    r"(?:\bor|\bto|\band)\s+\d[\d,]*(?:\.\d+)?(?!st\b|nd\b|rd\b|th\b)"
                    r"\s*(?:km|kilometers?|kilometres?|meters?|metres?|m|au|k)\b)",
                    after,
                    re.I,
                )
            ):
                pos = m.end()
                continue
            # Do not treat a structural label index (e.g. "Wave 2 - 40km", "Group 1 - 15km", "Wave 1 - 500m") as v1.
            if m.group(2) is None and m.group(3) is not None:
                struct_prefix = cleaned[max(0, m.start(1) - 32) : m.start(1)]
                if _STRUCT_DIST_PREFIX_RE.search(struct_prefix):
                    pos = m.start(3)
                    continue
            prefix = cleaned[max(0, m.start() - 24) : m.start()].lower()
            has_dist_ctx = bool(re.search(r"(?:within|proximity|range|distance|orbit|~|approx\.?|at|off)\s*$", prefix))
            has_time_ctx = bool(re.search(r"\b(?:after|every|wait|for|in|delay(?:ed)?(?:\s+by)?)\s*$", prefix))
            has_time_suffix = bool(re.match(r"^\s*(?:timer|delay|later|duration|mins?|minutes?)\b", after, re.I))
            v1_bad = unit1 == "m" and ((has_time_ctx and v1 <= 180) or has_time_suffix or (v1 < 100 and not has_dist_ctx))
            v2_bad = v2 is not None and unit2 == "m" and ((has_time_ctx and v2 <= 180) or has_time_suffix or (v2 < 100 and not has_dist_ctx))
            if v1_bad or v2_bad:
                pos = m.start(3) if (v1_bad and m.group(3) is not None and m.group(2) is not None) else m.end()
                continue
            lo = v1 * unit_scale[unit1]
            hi = (v2 * unit_scale[unit2]) if v2 is not None else lo
            if lo > hi:
                lo, hi = hi, lo
            min_m: int | float = int(round(lo)) if abs(lo - round(lo)) < 1e-6 else round(lo, 2)
            max_m: int | float = int(round(hi)) if abs(hi - round(hi)) < 1e-6 else round(hi, 2)
            return {"min_m": min_m, "max_m": max_m}
    return None


def _find_unparsed_distance(probe: str) -> str | None:
    """Return raw unparsed distance token if probe contains an ambiguous non-listed distance string."""
    cleaned = clean_wiki_inline(probe)
    for pat in _UNPARSED_DISTANCE_PATTERNS:
        for m in pat.finditer(cleaned):
            if m.lastindex == 2:
                lo, hi = int(m.group(1)), int(m.group(2))
                if hi < 15 or lo > hi:
                    continue
                tail = cleaned[m.end() : m.end() + 24].lower()
                head = cleaned[max(0, m.start() - 24) : m.start()].lower()
                if re.match(r"^\s*(?:ships?|npcs?|mins?|minutes?|secs?|seconds?|waves?|%)", tail):
                    continue
                if re.search(r"\b(?:total|score|fitness|waves?)\s*[:#]?\s*$", head):
                    continue
            return m.group(0).strip()
    return None


def extract_wave_distance_info(wave_name: str, group_name: str | None = None) -> dict[str, Any]:
    """Return parsed numeric distance or flag non-listed distance strings as distance_unparsed without guessing."""
    probes = [group_name, wave_name] if group_name else [wave_name]
    for probe in probes:
        if not probe:
            continue
        parsed = parse_wave_distance(probe)
        if parsed is not None:
            return {"distance_m": parsed, "distance_raw": None, "distance_unparsed": False}
        raw_unparsed = _find_unparsed_distance(probe)
        if raw_unparsed is not None:
            return {"distance_m": None, "distance_raw": raw_unparsed, "distance_unparsed": True}
    return {"distance_m": None, "distance_raw": None, "distance_unparsed": False}


def _extract_volume_and_strip(cleaned_text: str) -> tuple[float | None, str]:
    """Extract m3 volume from objective text and return (volume_m3, text_without_volume)."""
    vol: float | None = None
    m_vol = re.search(r"\(\s*([\d,.]+)\s*m\s*(?:\^?3)?\s*\)|([\d,.]+)\s*m\s*(?:\^?3)\b", cleaned_text, re.I)
    if m_vol:
        vol = _parse_number_token(m_vol.group(1) or m_vol.group(2))
    s_no_vol = re.sub(r"\(\s*[\d,.]+\s*m\s*(?:\^?3)?\s*\)|[\d,.]+\s*m\s*(?:\^?3)\b", " ", cleaned_text, flags=re.I)
    return vol, re.sub(r"\s+", " ", s_no_vol).strip()


def extract_courier_payload(objective_raw: str, sde: "SdeIndex") -> dict[str, Any] | None:
    """Extract courier cargo item name, count, volume m3, and SDE item type IDs."""
    s = clean_wiki_inline(objective_raw)
    if not s:
        return None
    vol, s_no_vol = _extract_volume_and_strip(s)

    qty: int | None = None
    item: str | None = None
    m_q = re.search(
        r"(?:deliver|transport|haul|move|pick\s*up(?:\s+and\s+deliver)?|bring(?:\s+back)?|fetch|carry|ac?quire(?:\s+these\s+goods)?)\s*:?\s*"
        r"(\d[\d,]*)\s*(?:x\s+)?(.+?)(?:\s+to\s+.+|\s+from\s+.+|$|[.;])",
        s_no_vol,
        re.I,
    )
    if m_q:
        qty = int(m_q.group(1).replace(",", ""))
        item = m_q.group(2).strip(" .,:;\"'")
    else:
        m_q2 = re.search(r"\b(\d[\d,]*)\s*x\s+([A-Za-z][^.;()]+)", s_no_vol, re.I)
        if m_q2:
            qty = int(m_q2.group(1).replace(",", ""))
            item = m_q2.group(2).strip(" .,:;\"'")
        else:
            m_it = re.search(
                r"(?:deliver|transport|haul|move|pick\s*up(?:\s+and\s+deliver)?|bring(?:\s+back)?|ac?quire(?:\s+these\s+goods)?)\s+"
                r"(?:the\s+|some\s+|these\s+goods:\s*)?(.+?)(?:\s+to\s+.+|\s+from\s+.+|\s+and\s+return\b|$|[.;])",
                s_no_vol,
                re.I,
            )
            if m_it:
                cand = m_it.group(1).strip(" .,:;\"'")
                if cand and "stranded pilot" not in cand.lower():
                    item = cand

    if not item and vol is None and qty is None:
        return None
    item_type_ids = sde.lookup_item_type_ids(item) if item else []
    return {
        "item": item,
        "quantity": qty,
        "volume_m3": vol,
        "item_type_ids": item_type_ids,
        "raw": s,
    }


def extract_mining_payload(objective_raw: str, sde: "SdeIndex") -> dict[str, Any] | None:
    """Extract mining ore/ice/gas name, units, volume m3, and SDE ore type IDs."""
    s = clean_wiki_inline(objective_raw)
    if not s:
        return None
    vol, s_no_vol = _extract_volume_and_strip(s)

    qty: int | None = None
    ore: str | None = None
    patterns = (
        r"(?:mine|harvest|ac?quire(?:\s+these\s+goods)?|reprocess(?:\s+it)?\s+(?:to|into)|bring\s+back)\s*:?\s*"
        r"(\d[\d,]*)\s*(?:x\s+)?(?:units\s+)?(?:of\s+)?([A-Za-z][A-Za-z0-9' \-]+?)"
        r"(?:\s+to\s+your\s+agent|\s+in\s+the|\s+and\s+|[.;()]|$)",
        r"(?:mine|harvest)\s+(?:all\s+the\s+|some\s+|enough\s+|out\s+all\s+the\s+|of\s+)?"
        r"([A-Za-z][A-Za-z0-9' \-]+?)(?:\s+ore)?(?:\s+in\s+the|\s+to\s+|\s+and\s+|[.;()]|$)",
    )
    for pat in patterns:
        m = re.search(pat, s_no_vol, re.I)
        if m:
            if m.lastindex == 2:
                qty = int(m.group(1).replace(",", ""))
                ore = m.group(2).strip(" .,:;\"'")
            else:
                ore = m.group(1).strip(" .,:;\"'")
            break
    if ore and ore.lower() in ("units", "ore", "units of", "until you find"):
        ore = None
    if ore and ore.lower().startswith("of "):
        ore = ore[3:].strip()
    if ore and ore.lower() == "green aristite":
        ore = "Green Arisite"
    if not ore and vol is None and qty is None:
        return None
    ore_type_ids = sde.lookup_item_type_ids(ore, mineable_only=True) if ore else []
    return {
        "ore_or_material": ore,
        "quantity": qty,
        "volume_m3": vol,
        "ore_type_ids": ore_type_ids,
        "raw": s,
    }


def extract_item_objective_payload(objective_raw: str, sde: "SdeIndex") -> dict[str, Any] | None:
    """Extract target item name, quantity, volume m3, and SDE type IDs from item-retrieval/trade prose."""
    s = clean_wiki_inline(objective_raw)
    if not s:
        return None
    vol, s_no_vol = _extract_volume_and_strip(s)
    qty: int | None = None
    item: str | None = None
    m_num = re.search(
        r"(?:^|\b)(?:retrieve|acquire(?:\s+these\s+goods)?|recover|bring(?:\s+back)?|fetch|loot|steal|"
        r"obtain|collect|find|pick\s+up|provide|buy|return|gather|trade|deliver|transport|haul|move|"
        r"place|manufacture|build|produce|reprocess)\s*:?\s*(?:the\s+)?"
        r"(\d[\d,]*)(?:\s*x\s+|\s+(?:units?\s+(?:of\s+)?)?)"
        r"([A-Za-z][A-Za-z0-9' \-/“”\"]+?)"
        r"(?:\s+(?:from|and\s+return|and\s+bring|to\s+the|to\s+your|dropped\s+by)\b|[.;()]|$)",
        s_no_vol,
        re.I,
    ) or re.search(
        r"^\s*(\d[\d,]*)\s*x\s+(?:units?\s+(?:of\s+)?)?"
        r"([A-Za-z][A-Za-z0-9' \-/“”\"]+?)"
        r"(?:\s+(?:from|and\s+return|and\s+bring|to\s+the|to\s+your|dropped\s+by)\b|[.;()]|$)",
        s_no_vol,
        re.I,
    )
    if m_num:
        qty = int(m_num.group(1).replace(",", ""))
        item = m_num.group(2).strip(" .,:;\"'")
    else:
        m_noun = re.search(
            r"\b(?:retrieve|acquire(?:\s+these\s+goods)?|recover|bring(?:\s+back)?|fetch|loot|steal|"
            r"obtain|collect|rescue|save|pick\s+up|provide|buy|return|gather)\s+"
            r"(?:the\s+|some\s+|all\s+)?([A-Za-z][A-Za-z0-9' \-/“”\"]+?)"
            r"(?:\s+(?:from|and\s+return|and\s+bring|to\s+the|to\s+your|dropped\s+by|in\s+the)\b|[.;()]|$)",
            s_no_vol,
            re.I,
        )
        if m_noun:
            item = m_noun.group(1).strip(" .,:;\"'")
    if item and item.lower() in ("goods", "item", "items", "cargo", "evidence", "data", "passengers", "prisoners"):
        item_type_ids: list[int] = []
    else:
        item_type_ids = sde.lookup_item_type_ids(item) if item else []
    if not item and vol is None and qty is None:
        return None
    return {
        "item": item,
        "quantity": qty,
        "volume_m3": vol,
        "item_type_ids": item_type_ids,
    }


_OBJ_STOPWORDS = frozenset(
    {
        "the", "and", "for", "all", "out", "you", "are", "his", "her", "its", "one", "two",
        "has", "was", "not", "but", "can", "who", "near", "from", "into", "with", "that",
        "this", "some", "them", "they", "will", "have", "your", "must", "off", "get",
    }
)
_OBJ_STRUCT_WORDS_RE = re.compile(
    r"\b(?:structure|tower|bunker|facility|station|repair\s+station|outpost|battery|sentry|sentries|"
    r"warehouse|depot|ammunitions?\s+depot|silo|habitat|base|beacon|generator|array|complex|"
    r"listening\s+post|lookout\s+post|reactor|stronghold|hq|headquarters|factory|refinery|"
    r"accelerator|hive|hangar|drone\s+hive|relay|research\s+station|chapel|"
    r"communications?\s+(?:tower|array|relay|outpost)|comms?\s+tower|power\s+grid)\b",
    re.I,
)
_OBJ_GENERIC_PRONOUNS = frozenset({"it", "them", "this", "that", "these", "those", "targets"})


def build_structured_objective(
    meta: dict[str, Any],
    courier_payload: dict[str, Any] | None,
    mining_payload: dict[str, Any] | None,
    rooms: list[dict[str, Any]],
    sde_match: dict[str, Any],
    sde: "SdeIndex",
    categories: list[str] | None = None,
) -> dict[str, Any]:
    """Build 3-state objective (explicit | inferred | unclassified) without default guessing."""
    obj_text = (meta.get("objective") or "").strip()
    mtype_low = (meta.get("mission_type") or meta.get("site_type") or "").strip().lower()
    cats_low = [c.strip().lower() for c in (categories or []) if c]
    matched_by_site_type = bool(
        (re.search(r"\b(?:data|relic)\b", mtype_low) and "data center" not in mtype_low)
        or "hacking" in mtype_low
        or "archaeolog" in mtype_low
    )
    matched_by_category = any(
        c in ("data sites", "relic sites") or "data site" in c or "relic site" in c for c in cats_low
    )
    obj_low = obj_text.lower()

    def _result(
        kind: str,
        classification_state: str,
        inference_rule: str | None = None,
        *,
        target_name: str | None = None,
        target_type_ids: list[int] | None = None,
        item_quantity: int | None = None,
        volume_m3: float | None = None,
    ) -> dict[str, Any]:
        assert kind in OBJECTIVE_KIND_ENUM
        assert classification_state in ("explicit", "inferred")
        if classification_state == "explicit":
            assert inference_rule is None
        else:
            assert inference_rule
        out: dict[str, Any] = {
            "kind": kind,
            "classification_state": classification_state,
            "inference_rule": inference_rule,
            "original_text": obj_text,
        }
        if target_name:
            out["target_name"] = target_name
        if target_type_ids is not None:
            out["target_type_ids"] = sorted(set(target_type_ids))
        if item_quantity is not None:
            out["item_quantity"] = item_quantity
        if volume_m3 is not None:
            out["volume_m3"] = volume_m3
        return out

    def _objective_spawns() -> list[dict[str, Any]]:
        hits: list[dict[str, Any]] = []
        for r in rooms:
            for w in r["waves"]:
                for sp in w["spawns"]:
                    tr = sp.get("trigger")
                    if not tr or tr.get("classification_state") == "unclassified" or tr.get("kind") != "objective_complete":
                        continue
                    orig_low = (tr.get("original_text") or "").lower()
                    if tr.get("unlocks_gate") and not re.search(
                        r"\b(?:mission\s+objective|mission\s+complete|completes?\s+(?:the\s+)?(?:mission|objectives?)|"
                        r"ends?\s+(?:the\s+)?(?:expedition|mission|site)|expedition\s+end|"
                        r"(?:kill|destroy)\s+to\s+complete)\b",
                        orig_low,
                    ):
                        continue
                    hits.append(sp)
        return hits

    def _match_target_in_rooms(target_phrase: str | None) -> tuple[str | None, list[int]]:
        if not target_phrase:
            return None, []
        ptoks = [
            t for t in re.findall(r"[a-z0-9]+", target_phrase.lower())
            if len(t) >= 3 and t not in _OBJ_STOPWORDS
        ]
        if not ptoks:
            return None, []
        best_score = 0
        best_name: str | None = None
        best_ids: list[int] = []
        for r in rooms:
            for w in r["waves"]:
                for sp in w["spawns"]:
                    rname = sp.get("raw_name") or ""
                    ids = sp["resolution"].get("type_ids") or []
                    if not rname or not ids:
                        continue
                    rwords = set(re.findall(r"[a-z0-9]+", rname.lower()))
                    score = sum(1 for tk in ptoks if tk in rwords)
                    if score > best_score:
                        best_score = score
                        best_name = rname
                        best_ids = ids
        if best_ids:
            return best_name, best_ids
        return target_phrase, sde.lookup_item_type_ids(target_phrase)

    if mining_payload is not None and ("mining" in mtype_low or bool(mining_payload.get("ore_type_ids"))):
        return _result(
            "mine_ore",
            "inferred",
            "missiondetails_mining_payload",
            target_name=mining_payload.get("ore_or_material"),
            target_type_ids=list(mining_payload.get("ore_type_ids") or []),
            item_quantity=mining_payload.get("quantity"),
            volume_m3=mining_payload.get("volume_m3"),
        )

    is_courier_mtype = ("courier" in mtype_low or "distribution" in mtype_low)
    combat_lead = bool(
        re.match(
            r"^\s*(?:destroy|kill|eliminate|defeat|clear|wipe\s+out|assassinate|neutralize|attack|hunt|ambush|intercept|engage)\b",
            obj_low,
        )
    )
    has_retrieve_cue = bool(
        re.search(
            r"\b(?:retrieve|acquire|recover|bring(?:\s+back)?|fetch|loot|steal|obtain|collect|rescue|pick\s+up|gather)\b"
            r"|\bbring\s+(?:the\s+|some\s+|all\s+|\d+\s*(?:x\s+)?)?[a-z0-9' \-]+\s+to\s+(?:your\s+)?agent\b"
            r"|^\s*\d+\s*x\s+",
            obj_low,
        )
    )
    is_retrieve_verb = bool(
        has_retrieve_cue
        or (not combat_lead and re.search(r"\b(?:provide|buy|return\s+\d+)\b", obj_low))
    )
    is_deliver_verb = bool(
        not (combat_lead and not is_courier_mtype)
        and re.search(
            r"\b(?:deliver|haul|drop\s+off|courier|transport\s+(?:the\s+|some\s+|all\s+|\d+|cargo|goods|passengers?|items?|supplies|freight))\b",
            obj_low,
        )
    )

    if courier_payload is not None:
        if is_retrieve_verb and not is_deliver_verb and not is_courier_mtype:
            return _result(
                "retrieve_item",
                "explicit",
                target_name=courier_payload.get("item"),
                target_type_ids=list(courier_payload.get("item_type_ids") or []),
                item_quantity=courier_payload.get("quantity"),
                volume_m3=courier_payload.get("volume_m3"),
            )
        return _result(
            "deliver_item",
            "inferred",
            "missiondetails_courier_payload",
            target_name=courier_payload.get("item"),
            target_type_ids=list(courier_payload.get("item_type_ids") or []),
            item_quantity=courier_payload.get("quantity"),
            volume_m3=courier_payload.get("volume_m3"),
        )

    if re.search(r"\b(?:choose\s+(?:a|between|which|one|your\s+path|next\s+mission)|branch(?:ing)?\s+(?:choice|point))\b", obj_low):
        return _result("branch_choice", "explicit")
    if "branch" in mtype_low:
        return _result("branch_choice", "inferred", "missiondetails_mission_type")

    if (
        re.search(
            r"^(?:report|repport|talk|speak|fly)\s+(?:to\s+)?(?:the\s+)?(?:epic\s+arc\s+)?agent\b|\bmission\s+is\s+purely\s+informational\b",
            obj_low,
        )
        or (
            not combat_lead
            and not re.search(r"\b(?:kill|destroy|ambush|hack|retrieve|deliver|mine)\b", obj_low)
            and bool(re.match(r"^(?:[Rr]eport|[Rr]epport|[Tt]alk|[Ss]peak)\s+[Tt]o\s+[A-Z][A-Za-z0-9' \-]+", obj_text))
        )
    ):
        return _result("talk_to_agent", "explicit")
    if mtype_low in ("talk to agent", "report to agent", "agent interaction"):
        return _result("talk_to_agent", "inferred", "missiondetails_mission_type")

    if not combat_lead and not re.search(r"\b(?:kill|destroy)\b", obj_low) and re.search(r"\b(?:manufacture|build|produce)\b", obj_low):
        item_pay = extract_item_objective_payload(obj_text, sde)
        return _result(
            "manufacture",
            "explicit",
            target_name=item_pay.get("item") if item_pay else None,
            target_type_ids=list(item_pay.get("item_type_ids") or []) if item_pay else None,
            item_quantity=item_pay.get("quantity") if item_pay else None,
            volume_m3=item_pay.get("volume_m3") if item_pay else None,
        )
    if "manufactur" in mtype_low or "industry" in mtype_low:
        return _result("manufacture", "inferred", "missiondetails_mission_type")

    if re.search(r"\b(?:hack|analy[sz]e)\b", obj_low):
        return _result("hack", "explicit")

    if is_deliver_verb:
        item_pay = extract_item_objective_payload(obj_text, sde)
        return _result(
            "deliver_item",
            "explicit",
            target_name=item_pay.get("item") if item_pay else None,
            target_type_ids=list(item_pay.get("item_type_ids") or []) if item_pay else None,
            item_quantity=item_pay.get("quantity") if item_pay else None,
            volume_m3=item_pay.get("volume_m3") if item_pay else None,
        )
    if is_courier_mtype:
        return _result("deliver_item", "inferred", "missiondetails_mission_type")

    obj_spawns = _objective_spawns()
    if (
        is_retrieve_verb
        and not re.search(r"\bbring\s+me\s+their\s+heads\b", obj_low)
        and not (
            combat_lead
            and not re.search(
                r"\b(?:retrieve|loot|recover|fetch|steal|bring\s+back|acquire|rescue|obtain|collect)\b"
                r"|\bbring\s+(?:the\s+|some\s+|all\s+|\d+\s*(?:x\s+)?)?[a-z0-9' \-]+\s+to\s+(?:your\s+)?agent\b",
                obj_low,
            )
        )
    ):
        item_pay = extract_item_objective_payload(obj_text, sde)
        target_nm = item_pay.get("item") if item_pay else None
        target_ids = list(item_pay.get("item_type_ids") or []) if item_pay else []
        if not target_ids:
            for sp in obj_spawns:
                for drop_txt in (sp.get("drop"), sp.get("cargo"), sp.get("loot")):
                    if drop_txt:
                        ids = sde.lookup_item_type_ids(drop_txt)
                        if ids:
                            target_ids.extend(ids)
                            if not target_nm:
                                target_nm = drop_txt
        return _result(
            "retrieve_item",
            "explicit",
            target_name=target_nm,
            target_type_ids=target_ids or None,
            item_quantity=item_pay.get("quantity") if item_pay else None,
            volume_m3=item_pay.get("volume_m3") if item_pay else None,
        )
    if "trade" in mtype_low or "bring item" in mtype_low:
        return _result("retrieve_item", "inferred", "missiondetails_mission_type")

    combat_obj_text = re.sub(r"\b(?:do\s+not|don't|never|avoid)\s+[^.;,)]*", "", obj_text, flags=re.I)
    combat_obj_low = combat_obj_text.lower()

    m_struct = re.search(
        r"\b(?:destroy|blow\s+up|demolish|level|eliminate|take\s+out)\s+(?:the\s+|all\s+(?:the\s+)?)?([^.;,]+)",
        combat_obj_text,
        re.I,
    )
    if m_struct:
        raw_obj_phrase = m_struct.group(1).strip()
        governed_phrase = re.split(
            r"\s+\b(?:near|around|at|by|in|from|within|on|guarding|defending|blockading|outside|inside|protecting)\b",
            raw_obj_phrase,
            maxsplit=1,
            flags=re.I,
        )[0].strip()
        if _OBJ_STRUCT_WORDS_RE.search(governed_phrase):
            t_nm, t_ids = _match_target_in_rooms(governed_phrase)
            return _result(
                "destroy_structure",
                "explicit",
                target_name=t_nm or governed_phrase,
                target_type_ids=t_ids or None,
            )

    if re.search(r"\b(?:kill|destroy|eliminate|defeat|clear|wipe\s+out)\s+(?:all\b|everything\b|every\b)", combat_obj_low) or re.search(
        r"\b(?:clear\s+(?:out\s+)?(?:the\s+|this\s+|all\s+|any\s+|remaining\s+)?(?:site|room|pocket|area|deadspace|zone|system|belt|outpost|base|complex|depot|station|facility|habitat|stronghold|sector|camp|bunker|pirates|enemies|hostiles|npcs|rats|drones|rogue\s+drones|waves?|spawns?|forces|defenders)|"
        r"(?:kill|destroy|eliminate|defeat)\s+(?:all\s+|any\s+)?(?:the\s+)?(?:(?:[a-z]+\s+)?ships\b|pirates|enemies|hostiles|defenders|guards|rats|drones|rogue\s+drones|merc(?:enary|enaries)|forces))\b",
        combat_obj_low,
    ):
        return _result("kill_all", "explicit")

    m_kill = re.search(
        r"\b(?:kill|destroy|eliminate|defeat|assassinate|neutralize|take\s+out|deal\s+with|terminate|intercept|engage|ambush)\s+"
        r"(?:the\s+)?([^.;,]+)",
        combat_obj_text,
        re.I,
    )
    if m_kill:
        t_phrase = m_kill.group(1).strip()
        tp_low = t_phrase.lower()
        first_tok = tp_low.split()[0] if tp_low else ""
        if tp_low and first_tok not in _OBJ_GENERIC_PRONOUNS:
            if re.search(r"\b(?:pirates|enemies|hostiles|defenders|guards|forces|waves?|all\s+ships)\b", tp_low) and len(tp_low.split()) <= 3:
                return _result("kill_all", "explicit")
            t_nm, t_ids = _match_target_in_rooms(t_phrase)
            return _result(
                "kill_target",
                "explicit",
                target_name=t_nm or t_phrase,
                target_type_ids=t_ids or None,
            )

    if re.search(
        r"^\s*(?:approach|fly\s+(?:to|near|within)|warp\s+to|investigate|reach|activate\s+the\s+gate|recon|scout\s+(?:the\s+|this\s+)?area)\b"
        r"|\b(?:approach\s+(?:the\s+)?[a-z0-9' \-]+|activate\s+the\s+gate)\b",
        obj_low,
    ):
        return _result("approach", "explicit")

    if re.search(r"^\s*(?:travel\s+to|go\s+to\s+(?:the\s+)?system)\b", obj_low):
        return _result("travel", "explicit")
    if mtype_low == "travel":
        return _result("travel", "inferred", "missiondetails_mission_type")

    if obj_spawns:
        def _is_hack_obj_spawn(sp: dict[str, Any]) -> bool:
            if (sp.get("raw_class") or "").strip().lower() in ("hackable", "hacking"):
                return True
            t_orig = (sp.get("trigger") or {}).get("original_text") or ""
            return bool(re.search(r"\bhack(?:ed|ing)?\b", t_orig, re.I) and not re.search(r"\b(?:kill|destroy)\b", t_orig, re.I))

        hack_spawns = [sp for sp in obj_spawns if _is_hack_obj_spawn(sp)]
        if hack_spawns and len(hack_spawns) == len(obj_spawns):
            t_ids = [tid for sp in hack_spawns for tid in (sp["resolution"].get("type_ids") or [])]
            t_name = hack_spawns[0].get("raw_name") or None
            total_qty = sum(sp["count"]["min"] for sp in hack_spawns if sp["count"].get("min") is not None) or None
            return _result(
                "hack",
                "inferred",
                "spawn_objective_complete_trigger",
                target_name=t_name,
                target_type_ids=t_ids or None,
                item_quantity=total_qty,
            )
        non_hack_spawns = [sp for sp in obj_spawns if not _is_hack_obj_spawn(sp)]
        struct_spawns = [
            sp for sp in non_hack_spawns
            if _is_structure_row(sp.get("raw_class", ""), sp.get("raw_name", ""))
        ]
        picked = struct_spawns or non_hack_spawns
        t_ids = [tid for sp in picked for tid in (sp["resolution"].get("type_ids") or [])]
        t_name = picked[0].get("raw_name") or None
        total_qty = sum(sp["count"]["min"] for sp in picked if sp["count"].get("min") is not None) or None
        return _result(
            "destroy_structure" if struct_spawns else "kill_target",
            "inferred",
            "spawn_objective_complete_trigger",
            target_name=t_name,
            target_type_ids=t_ids or None,
            item_quantity=total_qty,
        )

    for m in sde_match.get("missions", []):
        tpl = (m.get("content_template") or "").lower()
        if m.get("is_courier") or "couriermission" in tpl:
            return _result("deliver_item", "inferred", "sde_mission_template")
        if "trademission" in tpl or "heraldrymission" in tpl:
            return _result("retrieve_item", "inferred", "sde_mission_template")
        if "miningmission" in tpl:
            return _result("mine_ore", "inferred", "sde_mission_template")
        if "talktoagent" in tpl or "agentinteraction" in tpl:
            return _result("talk_to_agent", "inferred", "sde_mission_template")

    if matched_by_site_type:
        return _result("hack", "inferred", "cmbsite_data_relic_type")
    if matched_by_category:
        return _result("hack", "inferred", "wiki_data_relic_category")

    return {
        "kind": None,
        "classification_state": "unclassified",
        "inference_rule": None,
        "original_text": obj_text,
    }


def extract_forward_escalation_links(wikitext: str, page_title: str) -> list[str]:
    """Extract forward escalation target titles from Escalation sections and forward prose."""
    links: list[str] = []
    self_low = norm_title(page_title).lower()
    skip_titles = {
        "combat sites", "ded complexes", "expeditions", "unrated complexes",
        "combat anomalies", "data sites", "relic sites", "cosmic signatures",
    }

    def _add_candidate(raw_target: str) -> None:
        base_target = raw_target.split("|", 1)[0].split("#", 1)[0].replace("_", " ")
        t = clean_wiki_inline(base_target).strip(" .,:;")
        t = re.sub(r"^(?:Expeditions|Escalations?)\s*:\s*", "", t, flags=re.I).strip()
        if not t:
            return
        tl = t.lower()
        if tl == self_low or tl in skip_titles or ":" in t and tl.split(":", 1)[0] in ("category", "file", "image", "special"):
            return
        if re.match(
            r"^(?:escalation|expedition|note|warning|tips?|dead-?end|in\s+local|unknown|"
            r"on\s+(?:warp(?:\s*-?\s*in)?|escalation)|upon|destroying|killing|trigger\s+testimonial|"
            r"possible\s+escalation|failed\s+escalation|no\s+escalation)\b",
            tl,
        ) or re.search(r"\b(?:message|testimonial|failure|destruction)$", tl):
            return
        if t not in links:
            links.append(t)

    # 1. == Escalation == sections (MessageBox titles and [[wikilinks]])
    for m_sec in re.finditer(r"==+\s*Escalation[^=]*==+\s*\n(.*?)(?:\n==|\Z)", wikitext, re.S | re.I):
        sec_body = m_sec.group(1)
        for m_mb in re.finditer(r"\{\{\s*MessageBox\s*\|\s*([^|\n}]+)", sec_body, re.I):
            cand = m_mb.group(1).strip()
            m_wl = re.search(r"\[\[([^\]|]+)", cand)
            _add_candidate(m_wl.group(1) if m_wl else cand)
        for m_lk in re.finditer(
            r"(?:escalat(?:e|es|ed|ing|ion)\s+(?:into|to)|expedition\s+called)\s+(?:a\s+|an\s+|the\s+)?"
            r"((?:\[\[[^\]]+\]\]\s*(?:,\s*(?:or|and)\b|,|\bor\b|\band\b|/|(?=\s*\[\[))\s*)*\[\[[^\]|]+(?:\|[^\]]*)?\]\])",
            sec_body,
            re.I,
        ):
            for m_wl in re.finditer(r"\[\[([^\]|]+)", m_lk.group(1)):
                if not re.match(r"^ded\b", m_wl.group(1).strip(), re.I):
                    _add_candidate(m_wl.group(1))
        for m_pl in re.finditer(r"Escalat(?:es|ion)\s+to\s+([A-Z][^\n.<>{}\[\]]{3,65})", sec_body):
            cand = re.sub(r"\s*\{[^}]*\}.*$", "", m_pl.group(1)).strip()
            if not any(bad in cand.lower() for bad in ("ded", "complex", "variant")):
                _add_candidate(cand)

    # 2. Forward "escalates to/into [[...]]" and "escalation to/into [[...]]" prose anywhere in body (excluding "from [[...]]")
    for m_pro in re.finditer(
        r"(?:(?:can|may|will)?\s*escalat(?:e|es|ed|ing)|escalat(?:ion))\s+(?:into|to)\s+"
        r"(?:(?:either|a|an|the|\d+/\d+|ded\s+rated|faction|combat)\s+)*"
        r"((?:\[\[DED[^\]]*\]\]\s*)?\[\[[^\]|]+(?:\|[^\]]*)?\]\](?:\s*(?:,\s*(?:or|and)\b|,|\bor\b|\band\b|/)\s*\[\[[^\]|]+(?:\|[^\]]*)?\]\])*)",
        wikitext,
        re.I,
    ):
        for m_wl in re.finditer(r"\[\[([^\]|]+)", m_pro.group(1)):
            if not re.match(r"^ded\b", m_wl.group(1).strip(), re.I):
                _add_candidate(m_wl.group(1))

    return links


def iter_templates_and_headings(text: str) -> list[dict[str, Any]]:
    """Return ordered list of headings and balanced top-level/wrapped templates."""
    events: list[dict[str, Any]] = []
    for m in HEADING_RE.finditer(text):
        events.append(
            {
                "kind": "heading",
                "pos": m.start(),
                "level": len(m.group(1)),
                "text": clean_wiki_inline(m.group(2)),
            }
        )

    def scan_templates(src: str, base_pos: int) -> None:
        i, n = 0, len(src)
        while i < n:
            idx = src.find("{{", i)
            if idx == -1:
                break
            depth = 0
            j = idx
            while j < n:
                if src[j : j + 2] == "{{":
                    depth += 1
                    j += 2
                elif src[j : j + 2] == "}}":
                    depth -= 1
                    j += 2
                    if depth == 0:
                        body = src[idx + 2 : j - 2]
                        tname, pos_args, kv = split_template(body)
                        events.append(
                            {
                                "kind": "template",
                                "pos": base_pos + idx,
                                "name": tname,
                                "pos_args": pos_args,
                                "kv": kv,
                            }
                        )
                        # Unwrap container templates if they hold NPCTableRow / Missiondetails inside
                        if tname not in (
                            "npctablerow",
                            "npctr",
                            "npctablehead",
                            "npcth",
                            "npctableseparator",
                            "npcts",
                            "missiondetails",
                            "cmbsiteinfo",
                            "cmbsite",
                            "cmbsitebox",
                        ) and "{{" in body:
                            first_bar = body.find("|")
                            if first_bar != -1:
                                scan_templates(body[first_bar + 1 :], base_pos + idx + 2 + first_bar + 1)
                        i = j
                        break
                else:
                    j += 1
            else:
                # Unclosed "{{" must not discard remaining balanced templates later in the article.
                i = idx + 2

    scan_templates(text, 0)
    events.sort(key=lambda e: (e["pos"], 0 if e["kind"] == "heading" else 1))
    return events


def split_template(body: str) -> tuple[str, list[str], dict[str, str]]:
    """Split MediaWiki template call into normalized name, positional args, and named args."""
    parts: list[str] = []
    cur: list[str] = []
    d_brace = 0
    d_brack = 0
    k = 0
    n = len(body)
    while k < n:
        if body[k : k + 2] == "{{":
            d_brace += 1
            cur.append("{{")
            k += 2
        elif body[k : k + 2] == "}}":
            d_brace = max(0, d_brace - 1)
            cur.append("}}")
            k += 2
        elif body[k : k + 2] == "[[":
            d_brack += 1
            cur.append("[[")
            k += 2
        elif body[k : k + 2] == "]]":
            d_brack = max(0, d_brack - 1)
            cur.append("]]")
            k += 2
        elif body[k] == "|" and d_brace == 0 and d_brack == 0:
            parts.append("".join(cur))
            cur = []
            k += 1
        else:
            cur.append(body[k])
            k += 1
    parts.append("".join(cur))
    name = parts[0].strip().lower().replace("_", " ")
    kv: dict[str, str] = {}
    pos: list[str] = []
    for arg in parts[1:]:
        if "=" in arg and re.match(r"^\s*[A-Za-z0-9_ -]{1,40}=", arg):
            kk, vv = arg.split("=", 1)
            kv[kk.strip().lower()] = vv.strip()
        else:
            pos.append(arg.strip())
    return name, pos, kv


def _norm_class_bucket(npc_class: str) -> str | None:
    low = npc_class.lower()
    if "battlecruiser" in low or "battle cruiser" in low:
        return "battlecruiser"
    if "battleship" in low:
        return "battleship"
    if "destroyer" in low:
        return "destroyer"
    if "cruiser" in low:
        return "cruiser"
    if "frigate" in low:
        return "frigate"
    return None


STRUCTURE_CLASSES = {
    "structure",
    "sentry",
    "tower",
    "battery",
    "container",
    "hackable",
    "destructible object",
    "large collidable object",
    "hangar array",
    "beacon",
    "station",
    "deployable",
    "wreck",
    "object",
    "stargate",
    "turret",
    "refinery",
    "array",
    "warehouse",
    "depot",
    "outpost",
    "bunker",
    "silo",
}


def _is_structure_row(npc_class: str, raw_name: str = "") -> bool:
    cls_low = npc_class.strip().lower()
    if cls_low in STRUCTURE_CLASSES or cls_low in ("acceleration gate", "accl gate", "accel gate"):
        return True
    if not cls_low and raw_name:
        return bool(
            re.search(
                r"\b(?:tower|sentry|battery|bunker|gate|container|silo|refinery|shipyard|"
                r"outpost|station|depot|beacon|array|lab|habitat|habitation|monument|wreck|debris|cache|casket|strongbox)\b",
                raw_name,
                re.I,
            )
        )
    return False


def _allowed_cats_for_row(npc_class: str, raw_name: str = "") -> tuple[int, ...]:
    cls_low = npc_class.strip().lower()
    if cls_low in ("asteroid", "astroids", "ore", "ice"):
        return (25, 11, 2)
    if cls_low in ("gas cloud", "cloud", "agent", "celestial"):
        return (2, 11, 25)
    if cls_low in ("acceleration gate", "accl gate", "accel gate"):
        return GATE_ENTITY_CATEGORIES
    if _is_structure_row(npc_class, raw_name):
        return STRUCTURE_ENTITY_CATEGORIES
    return COMBAT_ENTITY_CATEGORIES


def _sde_ref_class_bucket(ref: "SdeTypeRef") -> str | None:
    return _norm_class_bucket(ref.group_name)


@dataclass
class SdeTypeRef:
    type_id: int
    name: str
    group_id: int
    category_id: int
    faction_id: int | None
    group_name: str


class SdeIndex:
    """In-memory lookup index built from local evomu_sde.sqlite and mission_ext."""

    def __init__(self, sde_db_path: Path, mission_ext_path: Path) -> None:
        if not sde_db_path.is_file() or sde_db_path.stat().st_size == 0:
            raise FileNotFoundError(f"SDE SQLite database missing or empty: {sde_db_path}")
        conn = sqlite3.connect(sde_db_path.resolve().as_uri() + "?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row

        groups = {
            r["group_id"]: r["name"]
            for r in conn.execute("SELECT group_id, name FROM sde_groups")
        }

        self.npc_by_cat_name: dict[int, dict[str, list[SdeTypeRef]]] = defaultdict(lambda: defaultdict(list))
        self.npc_by_cat_nospace: dict[int, dict[str, list[SdeTypeRef]]] = defaultdict(lambda: defaultdict(list))
        self.npc_by_cat_first_word_len: dict[int, dict[tuple[str, int], list[str]]] = defaultdict(
            lambda: defaultdict(list)
        )
        self.diamond_by_hull: dict[str, list[SdeTypeRef]] = defaultdict(list)
        self.item_by_name: dict[str, list[int]] = defaultdict(list)
        self.mineable_item_by_name: dict[str, list[int]] = defaultdict(list)
        self.faction_class_groups: dict[tuple[str, str], list[int]] = defaultdict(list)

        seen_first_words: dict[int, set[tuple[str, int, str]]] = defaultdict(set)
        for row in conn.execute(
            "SELECT type_id, group_id, category_id, name, faction_id, volume "
            "FROM sde_types WHERE name IS NOT NULL ORDER BY type_id"
        ):
            nl = row["name"].lower()
            tid = int(row["type_id"])
            self.item_by_name[nl].append(tid)
            cat_id = int(row["category_id"]) if row["category_id"] is not None else -1
            gname = groups.get(row["group_id"], "")
            if cat_id in (25, 4) or (cat_id == 2 and "cloud" in gname.lower()):
                self.mineable_item_by_name[nl].append(tid)
            if cat_id not in ALLOWED_SDE_CATEGORIES:
                continue
            ref = SdeTypeRef(
                type_id=int(row["type_id"]),
                name=row["name"],
                group_id=int(row["group_id"]),
                category_id=cat_id,
                faction_id=int(row["faction_id"]) if row["faction_id"] is not None else None,
                group_name=gname,
            )
            self.npc_by_cat_name[cat_id][nl].append(ref)
            self.npc_by_cat_nospace[cat_id][nl.replace(" ", "").replace("'", "")].append(ref)
            if cat_id == 11 and nl.startswith("♦ "):
                dhull = nl[2:].strip()
                if dhull:
                    self.diamond_by_hull[dhull].append(ref)
            toks = nl.split()
            if toks:
                key = (toks[0], len(toks), nl)
                if key not in seen_first_words[cat_id]:
                    seen_first_words[cat_id].add(key)
                    self.npc_by_cat_first_word_len[cat_id][(toks[0], len(toks))].append(nl)
            if cat_id == 11 and (
                gname.startswith("Mission ")
                or gname.startswith("Deadspace ")
                or gname.startswith("Asteroid ")
            ):
                fac = infer_faction_from_text(gname)
                if fac:
                    cls = _norm_class_bucket(gname)
                    if cls in ("frigate", "destroyer", "cruiser", "battlecruiser", "battleship"):
                        self.faction_class_groups[(fac, cls)].append(ref.type_id)

        for k in self.faction_class_groups:
            self.faction_class_groups[k] = sorted(set(self.faction_class_groups[k]))[:20]
        for cat_map in self.npc_by_cat_first_word_len.values():
            for k in cat_map:
                cat_map[k].sort()

        self.dungeons_by_id: dict[int, dict[str, Any]] = {}
        self.dungeons_by_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        self.dungeons_by_strict_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in conn.execute(
            "SELECT dungeon_id, name, archetype_id, faction_id, difficulty FROM sde_dungeons ORDER BY dungeon_id"
        ):
            d = dict(row)
            self.dungeons_by_id[int(d["dungeon_id"])] = d
            if d["name"]:
                for k in make_title_lookup_keys(d["name"], strict_only=True):
                    self.dungeons_by_strict_key[k].append(d)
                for k in make_title_lookup_keys(d["name"], strict_only=False):
                    self.dungeons_by_key[k].append(d)

        self.missions_by_id: dict[int, dict[str, Any]] = {}
        self.missions_by_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        self.missions_by_strict_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        mission_cols = {
            str(r[1])
            for r in conn.execute("PRAGMA table_info(sde_missions)").fetchall()
        }
        sel_cols = "mission_id, name, is_courier, lp_alpha, lp_omega, has_standing_rewards"
        if "content_template" in mission_cols:
            sel_cols += ", content_template"
        for row in conn.execute(
            f"SELECT {sel_cols} FROM sde_missions ORDER BY mission_id"
        ):
            m = dict(row)
            self.missions_by_id[int(m["mission_id"])] = m
            if m["name"]:
                for k in make_title_lookup_keys(m["name"], strict_only=True):
                    self.missions_by_strict_key[k].append(m)
                for k in make_title_lookup_keys(m["name"], strict_only=False):
                    self.missions_by_key[k].append(m)

        conn.close()

        self.mission_dungeons: dict[int, int] = {}
        if mission_ext_path.exists():
            ext_data = json.loads(mission_ext_path.read_text(encoding="utf-8"))
            for mid_str, did in ext_data.get("mission_dungeons", {}).items():
                self.mission_dungeons[int(mid_str)] = int(did)

    def lookup_item_type_ids(self, item_name: str | None, *, mineable_only: bool = False) -> list[int]:
        if not item_name:
            return []
        clean = item_name.strip().lower()
        cands = [clean]
        if clean.endswith("ies") and len(clean) > 4:
            cands.append(clean[:-3] + "y")
        elif clean.endswith("s") and len(clean) > 3:
            cands.append(clean[:-1])
        else:
            cands.append(clean + "s")

        crate_prefixes = (
            ("large crates of ", ("large crate of ", "large crates of ", "crate of ", "crates of ")),
            ("large crate of ", ("large crate of ", "large crates of ", "crate of ", "crates of ")),
            ("small crates of ", ("small crate of ", "small crates of ", "crate of ", "crates of ")),
            ("small crate of ", ("small crate of ", "small crates of ", "crate of ", "crates of ")),
            ("crates of ", ("crate of ", "crates of ", "large crate of ", "large crates of ")),
            ("crate of ", ("crate of ", "crates of ", "large crate of ", "large crates of ")),
        )
        qty_prefixes = ("groups of ", "group of ", "units of ")

        fallback_bare: list[str] = []
        matched_crate = False
        for prefix, out_prefixes in crate_prefixes:
            if clean.startswith(prefix):
                matched_crate = True
                sub = clean[len(prefix) :].strip()
                sub_forms = [sub]
                if sub.endswith("ies") and len(sub) > 4:
                    sub_forms.append(sub[:-3] + "y")
                elif sub.endswith("s") and len(sub) > 3:
                    sub_forms.append(sub[:-1])
                else:
                    sub_forms.append(sub + "s")
                for opf in out_prefixes:
                    for sf in sub_forms:
                        cands.append(f"{opf}{sf}")
                fallback_bare.extend(sub_forms)
                break

        if not matched_crate:
            for prefix in qty_prefixes:
                if clean.startswith(prefix):
                    sub = clean[len(prefix) :].strip()
                    cands.append(sub)
                    if sub.endswith("ies") and len(sub) > 4:
                        cands.append(sub[:-3] + "y")
                    elif sub.endswith("s") and len(sub) > 3:
                        cands.append(sub[:-1])
                    break

        table = self.mineable_item_by_name if mineable_only else self.item_by_name
        cands.extend(fallback_bare)
        for c in cands:
            if c in table:
                return sorted(set(table[c]))
        return []

    @staticmethod
    def normalize_variants(name: str, is_structure: bool = False) -> list[str]:
        cands = [name]
        low = name.lower()
        for a, b in SPELLING_PAIRS:
            if a in low:
                cands.append(re.sub(re.escape(a), b, name, flags=re.I).strip())

        compound = name
        for a, b in (
            ("defence", "defense"),
            ("batery", "battery"),
            ("batteries", "battery"),
            ("neutalizer", "neutralizer"),
            ("pithii", "pithi"),
            ("pithium ", "pithum "),
            ("centi ", "centii "),
            ("gistastis ", "gistatis "),
            ("gistatsis ", "gistatis "),
            ("gistatus ", "gistatis "),
            ("gistor ", "gistior "),
            ("gistium ", "gistum "),
            ("corelli ", "coreli "),
            ("colerior ", "corelior "),
            ("corelium ", "corelum "),
            ("corpier ", "corpior "),
            ("coprii ", "corpii "),
            ("martriarch", "matriarch"),
            ("matriarc", "matriarch"),
            ("ammater ", "ammatar "),
            ("war lord", "warlord"),
            ("sentinal", "sentinel"),
            ("blood raider", "bloodraider"),
            ("watchmen", "watchman"),
            ("gallent ", "gallente "),
            ("amarr navy ", "imperial navy "),
        ):
            compound = re.sub(re.escape(a), b, compound, flags=re.I)
        if compound != name:
            cands.append(compound)

        no_ast = re.sub(r"\s+(?:Asteroid|Ore)\b.*$", "", name, flags=re.I).strip()
        no_ast = re.sub(r"\s+(?:\d[\d,]*\s+units.*|\(total.*)$", "", no_ast, flags=re.I).strip()
        if no_ast and no_ast != name:
            cands.append(no_ast)

        extra: list[str] = []
        for c in list(cands):
            cl = c.lower()
            if cl.endswith("ies") and len(c) > 4:
                extra.append(c[:-3] + "y")
            elif cl.endswith("s") and not cl.endswith(("ss", "us", "is", "as")) and len(c) > 3:
                extra.append(c[:-1])
            elif not cl.endswith("s") and len(c) > 3:
                extra.append(c + "s")
        cands.extend(extra)

        stripped = name.strip(" .:*?!")
        while stripped.startswith("(") and stripped.count("(") > stripped.count(")"):
            stripped = stripped[1:].strip()
        while stripped.startswith("[") and stripped.count("[") > stripped.count("]"):
            stripped = stripped[1:].strip()
        while stripped.endswith(")") and stripped.count(")") > stripped.count("("):
            stripped = stripped[:-1].strip()
        while stripped.endswith("]") and stripped.count("]") > stripped.count("["):
            stripped = stripped[:-1].strip()
        if stripped != name:
            cands.append(stripped)
        unwrapped_prefix = re.sub(r"^\(([A-Za-z0-9' -]+)\)\s+", r"\1 ", stripped).strip()
        if unwrapped_prefix and unwrapped_prefix != stripped:
            cands.append(unwrapped_prefix)
        if stripped.lower().startswith("the "):
            cands.append(stripped[4:])
        else:
            cands.append("The " + stripped)

        no_paren = re.sub(r"\s*[\(\[][^)\]]*[\)\]]", "", stripped).strip()
        if no_paren and no_paren != stripped:
            cands.append(no_paren)

        for pf in (
            "Blood Raiders ",
            "Blood Raider ",
            "Serpentis Corporation ",
            "Serpentis ",
            "Angel Cartel ",
            "Guristas Pirates ",
            "Guristas ",
            "Sansha's Nation ",
            "Sansha ",
            "True Power ",
            "True Power' ",
            "Archangels ",
            "Archangel ",
            "EDENCOM ",
            "Core Runner ",
            "CHAIN ",
        ):
            if stripped.lower().startswith(pf.lower()):
                rest = stripped[len(pf) :].strip()
                if rest:
                    cands.append(rest)

        if low.startswith("corpum dark ") or low.startswith("corpum shadow "):
            cands.append("Dark " + stripped)
        if low.startswith("elder corpum dark ") or low.startswith("elder corpum shadow "):
            cands.append("Dark " + stripped[6:])
        if "-" in name:
            cands.append(name.replace("-", " "))
            cands.append(name.replace(" - ", "-"))

        if is_structure:
            cands.append(stripped + " Ship")
            cands.append(stripped + " Gun")
            cands.append(stripped + " Battery")
            cands.append("Deadspace " + stripped)
            m_rn = re.match(r"^(.+?)\s+(I|II|III|IV|V)$", stripped)
            if m_rn:
                cands.append(f"{m_rn.group(1)} Sentry {m_rn.group(2)}")
                m_tw = re.match(r"^(\S+)\s+Tower\s+(I|II|III)$", stripped, re.I)
                if m_tw:
                    cands.append(f"Tower Sentry {m_tw.group(1)} {m_tw.group(2)}")

        out: list[str] = []
        seen: set[str] = set()
        for c in cands:
            cl = c.strip().lower()
            if cl and cl not in seen:
                seen.add(cl)
                out.append(c.strip())
        return out

    @staticmethod
    def _filter_by_expected_class(
        refs: list[SdeTypeRef],
        expected_class: str | None,
    ) -> list[SdeTypeRef]:
        if not expected_class:
            return refs
        out: list[SdeTypeRef] = []
        for r in refs:
            rc = _sde_ref_class_bucket(r)
            if rc is not None:
                if rc == expected_class:
                    out.append(r)
                continue
            g_low = r.group_name.lower()
            if expected_class in ("frigate", "destroyer", "cruiser", "battlecruiser", "battleship") and any(
                tok in g_low for tok in ("structure", "sentry", "beacon", "object", "container", "wreck")
            ):
                continue
            out.append(r)
        return out

    def direct_hits(
        self,
        name: str,
        allow_fuzzy: bool = True,
        allowed_cats: tuple[int, ...] = COMBAT_ENTITY_CATEGORIES,
        expected_class: str | None = None,
        is_structure: bool = False,
    ) -> list[SdeTypeRef]:
        variants = self.normalize_variants(name, is_structure=is_structure)
        for cat in allowed_cats:
            by_name = self.npc_by_cat_name.get(cat, {})
            by_nospace = self.npc_by_cat_nospace.get(cat, {})
            for v in variants:
                vl = v.lower()
                if vl in by_name:
                    return by_name[vl]
                ns = vl.replace(" ", "").replace("'", "")
                if ns in by_nospace:
                    return by_nospace[ns]

        # Verified Category 11 ♦ Diamond NPC entities (explicit NPC corp prefixes or exact "Orca")
        if 11 in allowed_cats and self.diamond_by_hull:
            nl_d = name.strip().lower()
            if nl_d == "orca" and "orca" in self.diamond_by_hull:
                return self.diamond_by_hull["orca"]
            for corp_pf in (
                "imperial war reserves ",
                "state military stockpile ",
                "federal strategic materiel ",
                "republic fleet ordnance ",
                "blood raiders ",
                "serpentis corporation ",
                "archangels ",
                "true power ",
            ):
                if nl_d.startswith(corp_pf):
                    dhull = nl_d[len(corp_pf) :].strip()
                    if dhull in self.diamond_by_hull:
                        filtered = self._filter_by_expected_class(self.diamond_by_hull[dhull], expected_class)
                        if filtered:
                            return filtered

        if not allow_fuzzy:
            return []

        nl = name.strip().lower()
        while nl.startswith("(") and nl.count("(") > nl.count(")"):
            nl = nl[1:].strip()
        while nl.endswith(")") and nl.count(")") > nl.count("("):
            nl = nl[:-1].strip()
        toks = nl.split()

        for cat in allowed_cats:
            by_name = self.npc_by_cat_name.get(cat, {})
            by_fw = self.npc_by_cat_first_word_len.get(cat, {})

            # Deadspace cruiser missing fixed sub-tier token ("Corpum Engraver" -> "Corpum Arch Engraver", "Corelum Scout" -> "Corelum Chief Scout")
            if cat == 11 and len(toks) >= 2 and expected_class in (None, "cruiser"):
                for pfx, tier_w in (("corpum", "arch"), ("corelum", "chief")):
                    if toks[0] == pfx and toks[1] != tier_w:
                        cand_t = f"{pfx} {tier_w} " + " ".join(toks[1:])
                        if cand_t in by_name:
                            filtered = self._filter_by_expected_class(by_name[cand_t], expected_class)
                            if filtered:
                                return filtered

            # Unambiguous trailing token abbreviation (e.g. "Corelum Guardian Chief Infa/Saf/Sen/Def/Prot")
            if len(toks) >= 2 and len(toks[-1]) >= 3:
                lead = " ".join(toks[:-1]) + " "
                last_t = toks[-1]
                abbr = [
                    v
                    for k, v in by_name.items()
                    if k.startswith(lead) and k[len(lead) :].startswith(last_t)
                ]
                if abbr:
                    flat = self._filter_by_expected_class([x for m in abbr for x in m], expected_class)
                    if flat and len({x.name for x in flat}) <= 3:
                        return flat

            # Sibling rank substitution constrained strictly to the same (faction_family, ship_class)
            for prefix_len in (2, 1):
                if len(toks) > prefix_len:
                    pref_key = " ".join(toks[:prefix_len])
                    meta = RANK_PREFIX_META.get(pref_key)
                    if meta:
                        fam, src_cls = meta
                        if src_cls != "any" and expected_class and src_cls != expected_class:
                            break
                        target_cls = expected_class or (None if src_cls == "any" else src_cls)
                        rest = " ".join(toks[prefix_len:])
                        cand_prefixes: list[str] = []
                        if target_cls:
                            cand_prefixes.extend(FAMILY_CLASS_PREFIXES.get((fam, target_cls), []))
                            if src_cls == "any":
                                cand_prefixes.extend(FAMILY_CLASS_PREFIXES.get((fam, "any"), []))
                        else:
                            cand_prefixes.extend(FAMILY_CLASS_PREFIXES.get((fam, "any"), []))
                        for alt in cand_prefixes:
                            if alt == pref_key:
                                continue
                            cand = f"{alt} {rest}"
                            if cand in by_name:
                                filtered = self._filter_by_expected_class(by_name[cand], expected_class)
                                if filtered:
                                    return filtered
                        break

            # Rogue Drone suffix substitution constrained by ship_class (and hull class of suffix when expected_class is None)
            if len(toks) >= 2 and toks[-1] in ALL_ROGUE_DRONE_SUFFIXES:
                body_p = " ".join(toks[:-1])
                src_drone_cls = ROGUE_DRONE_SUFFIX_TO_CLASS.get(toks[-1])
                eff_cls = expected_class or src_drone_cls
                suf_pool = ROGUE_DRONE_SUFFIXES_BY_CLASS.get(eff_cls, (toks[-1],)) if eff_cls else (toks[-1],)
                for suf in suf_pool:
                    for cand in (f"{body_p} {suf}", f"strain {body_p} {suf}"):
                        if cand in by_name:
                            filtered = self._filter_by_expected_class(by_name[cand], expected_class)
                            if filtered:
                                return filtered

            # Guarded 1-typo difflib match: exact first word, same token count, single differing
            # non-Roman/non-digit token >= 4 chars with tight per-token similarity (>= 0.78)
            roman_or_num = {"i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"}
            if len(toks) >= 2:
                for base in (
                    nl,
                    re.sub(
                        r"\bmartriarch\b",
                        "matriarch",
                        re.sub(
                            r"\bgist(?:astis|atsis|atus)\b",
                            "gistatis",
                            re.sub(r"amarr navy ", "imperial navy ", re.sub(r"pithii", "pithi", nl)),
                        ),
                    ),
                ):
                    btoks = base.split()
                    pool = by_fw.get((btoks[0], len(btoks)), [])
                    if pool:
                        matches = difflib.get_close_matches(base, pool, n=5, cutoff=0.85)
                        for m_cand in matches:
                            ctoks = m_cand.split()
                            diffs = [(a, b) for a, b in zip(btoks, ctoks) if a != b]
                            if len(diffs) == 1:
                                ta, tb = diffs[0]
                                if ta in roman_or_num or tb in roman_or_num or ta.isdigit() or tb.isdigit():
                                    continue
                                if min(len(ta), len(tb)) < 4:
                                    continue
                                if difflib.SequenceMatcher(None, ta, tb).ratio() < 0.78:
                                    continue
                            elif len(diffs) > 1:
                                continue
                            filtered = self._filter_by_expected_class(by_name[m_cand], expected_class)
                            if filtered:
                                return filtered
        return []

    def expand_short_names(
        self,
        raw_name: str,
        allowed_cats: tuple[int, ...] = COMBAT_ENTITY_CATEGORIES,
        expected_class: str | None = None,
        is_structure: bool = False,
    ) -> tuple[list[str], bool]:
        """Expand short-form EVE NPC names (slashes, shared prefix/suffix/circumfix, ranges, ellipses)."""
        s = clean_wiki_inline(raw_name)
        if not s or set(s) <= set("?-.* "):
            return [], False
        had_ellipsis = "..." in s

        def strip_unbalanced(tok: str) -> str:
            t = tok.strip()
            while t.startswith("(") and t.count("(") > t.count(")"):
                t = t[1:].strip()
            while t.startswith("[") and t.count("[") > t.count("]"):
                t = t[1:].strip()
            while t.endswith(")") and t.count(")") > t.count("("):
                t = t[:-1].strip()
            while t.endswith("]") and t.count("]") > t.count("["):
                t = t[:-1].strip()
            return t

        def has_rank_or_faction_prefix(tok: str) -> bool:
            tl = tok.lower()
            return any(tl.startswith(rp + " ") for rp in RANK_PREFIX_META) or any(
                tl.startswith(npf + " ") for npf in FACTION_WORD_PREFIXES
            )

        def has_hit(cand: str, allow_fuzzy: bool = False) -> bool:
            return bool(
                self.direct_hits(
                    cand,
                    allow_fuzzy=allow_fuzzy,
                    allowed_cats=allowed_cats,
                    expected_class=expected_class,
                    is_structure=is_structure,
                )
            )

        # Numeric range brackets e.g. "AEGIS No. [1-4] Capital Construction Forge"
        m_rng = re.match(r"^(.*?)\[(\d+)\s*-\s*(\d+)\](.*)$", s)
        if m_rng and not has_hit(s, allow_fuzzy=False):
            a, b = int(m_rng.group(2)), int(m_rng.group(3))
            if 0 < b - a <= 12:
                expanded = [
                    re.sub(r"\s+", " ", f"{m_rng.group(1)}{k}{m_rng.group(4)}").strip()
                    for k in range(a, b + 1)
                ]
                if any(has_hit(x, allow_fuzzy=True) for x in expanded):
                    return expanded, had_ellipsis

        # Embedded bracket/paren override e.g. "Seven Recruit [Coreli Watchman/Patroller]" or "Chemical Factory (Fortified Angel Bunker)"
        m_emb = re.match(r"^[^\[\(]+([\[\(])([^\]\)]+)([\]\)])\s*$", s)
        if m_emb and not has_hit(s, allow_fuzzy=False):
            inner = m_emb.group(2).strip()
            if not re.search(
                r"\b(?:warning|km|units|m3|triggers|wave|boss|commander|spawn|note|optional|rare|maybe)\b",
                inner,
                re.I,
            ):
                exp_inner, _ = self.expand_short_names(
                    inner,
                    allowed_cats=allowed_cats,
                    expected_class=expected_class,
                    is_structure=is_structure,
                )
                if exp_inner and any(has_hit(x, allow_fuzzy=True) for x in exp_inner):
                    return exp_inner, had_ellipsis

        def expand_piece(piece: str) -> list[str]:
            piece = strip_unbalanced(piece)
            if not piece:
                return []
            m_cnt = re.match(r"^(?:\d+x\s+|\d+\s*-\s*\d+\s*x?\s+)(.+)$", piece, re.I)
            if m_cnt and not has_hit(piece, allow_fuzzy=False):
                piece = m_cnt.group(1).strip()
            if "/" not in piece and has_hit(piece, allow_fuzzy=False):
                return [piece]
            if "/" in piece and any(piece.lower() in self.npc_by_cat_name.get(cat, {}) for cat in allowed_cats):
                return [piece]

            # Parenthetical alternation: "Pithatis (Enforcer/Executor)" or "(Dire Pithi Infiltrator/Invader)"
            m = re.match(r"^(.*?)\(([^()]*?/[^()]*?)\)(.*)$", piece)
            if m:
                pre, inside, post = m.group(1), m.group(2), m.group(3)
                if not pre.strip() and not post.strip():
                    return expand_piece(inside)
                out: list[str] = []
                for opt in inside.split("/"):
                    opt = opt.strip()
                    if opt and opt != "...":
                        out.extend(expand_piece(re.sub(r"\s+", " ", f"{pre}{opt}{post}").strip()))
                return out

            if "/" in piece:
                parts = [strip_unbalanced(p) for p in piece.split("/") if strip_unbalanced(p)]
                # Hyphenated prefix e.g. "Type-A/B/C/D Anomalous Alvi"
                m_hyp = re.match(r"^([A-Za-z0-9]+-)([A-Za-z0-9]+)$", parts[0])
                if m_hyp and len(parts) >= 2:
                    hyp_pref = m_hyp.group(1)
                    tail_w = parts[-1].split()
                    suffix = (" " + " ".join(tail_w[1:])) if len(tail_w) > 1 else ""
                    hyp_cands = [f"{parts[0]}{suffix}"]
                    for idx, p in enumerate(parts[1:], 1):
                        tok = p.split()[0] if idx == len(parts) - 1 and suffix else p
                        hyp_cands.append(f"{hyp_pref}{tok}{suffix}")
                    if sum(1 for c in hyp_cands if has_hit(c, allow_fuzzy=False)) >= 2:
                        return hyp_cands

                # Ellipsis trailing suffix hint e.g. "Silverfish/... Alvi"
                last_ell = re.match(r"^\.\.\.\s*(.+)$", parts[-1])
                suffix_hint = last_ell.group(1).strip() if last_ell else ""
                if suffix_hint:
                    expanded = []
                    for p in parts[:-1]:
                        if p != "...":
                            expanded.append(
                                f"{p} {suffix_hint}".strip()
                                if not p.lower().endswith(suffix_hint.lower())
                                else p
                            )
                    return expanded

                parts = [strip_unbalanced(re.sub(r"^\.\.\.\s*|\s*\.\.\.$", "", p)) for p in parts]
                parts = [p for p in parts if p]
                if not parts:
                    return []
                if len(parts) == 1:
                    return [parts[0]]

                p0_w = parts[0].split()
                pl_w = parts[-1].split()

                # Run strict-first then fuzzy across circumfix, shared prefix, rolling prefix, and shared suffix
                # BEFORE accepting independent unprefixed parts (preventing "EoM Hydra/Ogre" -> unprefixed hits).
                for fuzzy_pass in (False, True):
                    # Circumfix: shared leading prefix AND shared trailing suffix ("Corelatis Squad/Wing Leader")
                    if len(p0_w) >= 2 and len(pl_w) >= 2:
                        best: list[str] | None = None
                        best_hits = 0
                        for kp in range(1, len(p0_w)):
                            prefix = " ".join(p0_w[:kp])
                            for ks in range(1, len(pl_w)):
                                suffix = " ".join(pl_w[-ks:])
                                cands = []
                                for idx, p in enumerate(parts):
                                    cand = p
                                    if (
                                        idx > 0
                                        and not cand.lower().startswith(prefix.lower())
                                        and not has_rank_or_faction_prefix(cand)
                                        and not cand.lower().startswith(suffix.lower())
                                    ):
                                        cand = f"{prefix} {cand}"
                                    if (
                                        idx < len(parts) - 1
                                        and not cand.lower().endswith(suffix.lower())
                                        and not cand.lower().startswith(suffix.lower() + " ")
                                    ):
                                        cand = f"{cand} {suffix}"
                                    cands.append(cand)
                                hits = sum(1 for c in cands if has_hit(c, allow_fuzzy=fuzzy_pass))
                                if hits > best_hits:
                                    best_hits = hits
                                    best = cands
                        if best and best_hits == len(best):
                            return best

                    # Rolling prefix ("Gistii Ambusher/Arch Gistii Outlaw/Thug") & Shared prefix ("EoM Hydra/Ogre")
                    best_pref: list[str] | None = None
                    best_pref_hits = 0
                    for kp in range(len(p0_w) - 1, 0, -1):
                        prefix = " ".join(p0_w[:kp])
                        cands = [parts[0]]
                        cur_prefix = prefix
                        for p in parts[1:]:
                            pl_tok = p.lower()
                            if pl_tok.startswith(cur_prefix.lower() + " ") or pl_tok.startswith(prefix.lower() + " "):
                                cands.append(p)
                                pw = p.split()
                                if len(pw) >= 2:
                                    cur_prefix = " ".join(pw[:-1])
                            elif has_rank_or_faction_prefix(p):
                                cands.append(p)
                                pw = p.split()
                                if len(pw) >= 2:
                                    cur_prefix = " ".join(pw[:-1])
                            else:
                                cands.append(f"{cur_prefix} {p}")
                        hits = sum(1 for c in cands if has_hit(c, allow_fuzzy=fuzzy_pass))
                        if hits > best_pref_hits:
                            best_pref_hits = hits
                            best_pref = cands
                    if best_pref and best_pref_hits == len(best_pref):
                        return best_pref

                    # Shared suffix ("Heavy/Light Missile Battery", "Render/Decimator Alvi", "Predator Alvior/ Marauder")
                    best_suf: list[str] | None = None
                    best_suf_hits = 0
                    suf_candidates: list[str] = [" ".join(pl_w[-ks:]) for ks in range(len(pl_w) - 1, 0, -1)]
                    if len(pl_w) == 1 and len(p0_w) >= 2:
                        suf_candidates.extend(" ".join(p0_w[-ks:]) for ks in range(len(p0_w) - 1, 0, -1))
                    for suffix in suf_candidates:
                        cands = []
                        suf_low = suffix.lower()
                        for p in parts:
                            pl_tok = p.lower()
                            if pl_tok.endswith(suf_low) or pl_tok.startswith(suf_low + " "):
                                cands.append(p)
                            else:
                                cands.append(f"{p} {suffix}")
                        hits = sum(1 for c in cands if has_hit(c, allow_fuzzy=fuzzy_pass))
                        if hits > best_suf_hits:
                            best_suf_hits = hits
                            best_suf = cands
                    if best_suf and best_suf_hits == len(best_suf):
                        return best_suf

                # Mixed prefix/suffix flip e.g. "Alvus Controller / Spearhead/Swarm Preserver Alvus"
                if len(parts) >= 2 and p0_w[0].lower() in ("alvus", "strain") and pl_w[-1].lower() in ALL_ROGUE_DRONE_SUFFIXES:
                    suf_word = pl_w[-1]
                    mixed_cands = [parts[0]]
                    for p in parts[1:]:
                        if p.lower().startswith(suf_word.lower() + " ") or p.lower().endswith(" " + suf_word.lower()):
                            mixed_cands.append(p)
                        else:
                            mixed_cands.append(f"{p} {suf_word}")
                    if sum(1 for c in mixed_cands if has_hit(c, allow_fuzzy=True)) >= 2:
                        return mixed_cands

                # Independent full names separated by "/" (only if no shared affix resolved all parts)
                if all(has_hit(p, allow_fuzzy=True) for p in parts):
                    return parts

                if best_suf and best_suf_hits > best_pref_hits:
                    return best_suf
                if best_pref and best_pref_hits >= 2:
                    return best_pref
                if best_pref and best_pref_hits == 1 and not has_hit(parts[-1], allow_fuzzy=True):
                    return best_pref
                return parts

            return [piece]

        if "," in s and not has_hit(s, allow_fuzzy=False):
            comma_parts = [x.strip() for x in s.split(",") if x.strip()]
            cand_list: list[str] = []
            for cp in comma_parts:
                cand_list.extend(expand_piece(cp))
            if len(cand_list) > 1 and any(has_hit(x, allow_fuzzy=True) for x in cand_list):
                return cand_list, had_ellipsis
        return expand_piece(s), had_ellipsis

    def find_family_candidates(
        self,
        name: str,
        allowed_cats: tuple[int, ...] = COMBAT_ENTITY_CATEGORIES,
    ) -> list[SdeTypeRef]:
        low = clean_wiki_inline(name).strip(" .?*!").lower()
        low = re.sub(r"\s*[\(\[][^)\]]*[\)\]]", "", low).strip()
        if low.endswith("pithii"):
            low = low[:-1]
        if low == "guardian angel":
            low = "guardian angels"
        if not low:
            return []
        if low in BARE_FAMILY_PREFIXES or (
            len(low.split()) >= 2 and any(low.startswith(p + " ") for p in BARE_FAMILY_PREFIXES)
        ):
            pref = "federation praktor " if low == "federation prak" else low + " "
            hits: list[SdeTypeRef] = []
            for cat in allowed_cats:
                by_name = self.npc_by_cat_name.get(cat, {})
                for k in sorted(by_name):
                    if k.startswith(pref):
                        hits.extend(by_name[k])
                if hits:
                    return hits[:20]
        return []

    def resolve_npc_row(
        self,
        npc_class: str,
        raw_name: str,
        note: str,
        page_factions: list[str],
        is_event: bool = False,
        include_name_map: bool = False,
    ) -> dict[str, Any]:
        raw_clean = clean_wiki_inline(raw_name)
        cls_low = npc_class.strip().lower()
        allowed_cats = _allowed_cats_for_row(npc_class, raw_clean)
        is_struct = _is_structure_row(npc_class, raw_clean)
        cb = _norm_class_bucket(npc_class)

        # Acceleration Gate rows (even when the wiki name cell is blank)
        if cls_low in ("acceleration gate", "accl gate", "accel gate") or (
            raw_clean and re.match(r"^(?:acceleration )?gate\b", raw_clean.lower())
        ):
            hits = self.direct_hits(
                "Acceleration Gate",
                allow_fuzzy=False,
                allowed_cats=GATE_ENTITY_CATEGORIES,
                is_structure=True,
            )
            if hits:
                return {
                    "status": "resolved_exact",
                    "method": "acceleration_gate",
                    "expanded_names": [raw_clean or "Acceleration Gate"],
                    "type_ids": sorted({h.type_id for h in hits}),
                    "candidate_type_ids": sorted({h.type_id for h in hits}),
                    "matched_names": sorted({h.name for h in hits}),
                    "unresolved_parts": [],
                    "reason": None,
                }

        if not raw_clean:
            return {
                "status": "unresolved",
                "method": "none",
                "expanded_names": [],
                "type_ids": [],
                "candidate_type_ids": [],
                "matched_names": [],
                "unresolved_parts": [],
                "reason": "blank_name_in_wiki_table",
            }
        if set(raw_clean) <= set("?-.* "):
            return {
                "status": "unresolved",
                "method": "none",
                "expanded_names": [],
                "type_ids": [],
                "candidate_type_ids": [],
                "matched_names": [],
                "unresolved_parts": [raw_clean],
                "reason": "placeholder_question_mark",
            }

        # Asteroid / ore rows
        if cls_low == "asteroid":
            m_ast = ORE_STRIP_RE.match(raw_clean)
            if m_ast:
                hits = self.direct_hits(
                    m_ast.group(1),
                    allow_fuzzy=False,
                    allowed_cats=ASTEROID_ENTITY_CATEGORIES,
                    is_structure=False,
                )
                if hits:
                    return {
                        "status": "resolved_structure_faction",
                        "method": "asteroid_ore_name",
                        "expanded_names": [m_ast.group(1)],
                        "type_ids": sorted({h.type_id for h in hits}),
                        "candidate_type_ids": sorted({h.type_id for h in hits}),
                        "matched_names": sorted({h.name for h in hits}),
                        "unresolved_parts": [],
                        "reason": None,
                    }

        names, had_ellipsis = self.expand_short_names(
            raw_name,
            allowed_cats=allowed_cats,
            expected_class=cb,
            is_structure=is_struct,
        )
        if not names:
            return {
                "status": "unresolved",
                "method": "none",
                "expanded_names": [],
                "type_ids": [],
                "candidate_type_ids": [],
                "matched_names": [],
                "unresolved_parts": [raw_clean],
                "reason": "placeholder_question_mark",
            }

        exact_hits: list[SdeTypeRef] = []
        struct_hits: list[SdeTypeRef] = []
        fam_hits: list[SdeTypeRef] = []
        unresolved_parts: list[str] = []

        for nm in names:
            nm_cats = _allowed_cats_for_row(npc_class, nm)
            nm_struct = is_struct or _is_structure_row(npc_class, nm)
            hits = self.direct_hits(
                nm,
                allow_fuzzy=True,
                allowed_cats=nm_cats,
                expected_class=cb,
                is_structure=nm_struct,
            )
            if hits:
                exact_hits.extend(hits)
                continue

            if note:
                note_matched = False
                m_ren = re.search(
                    r"(?:renamed(?:\s+to|\s+from)?|formerly|previously|now|ship\s+type:?)\s*['\"]?([A-Za-z0-9' /\-]+)",
                    note,
                    re.I,
                )
                note_cands: list[str] = []
                if m_ren:
                    note_cands.append(m_ren.group(1).strip())
                elif "/" in note or len(note.split()) <= 4:
                    note_cands.append(note.strip())
                for nc in note_cands:
                    exp_nc, _ = self.expand_short_names(
                        nc,
                        allowed_cats=nm_cats,
                        expected_class=cb,
                        is_structure=nm_struct,
                    )
                    nc_hits: list[SdeTypeRef] = []
                    for enc in exp_nc:
                        h = self.direct_hits(
                            enc,
                            allow_fuzzy=bool(m_ren),
                            allowed_cats=nm_cats,
                            expected_class=cb,
                            is_structure=nm_struct,
                        )
                        if not h:
                            nc_hits = []
                            break
                        nc_hits.extend(h)
                    if nc_hits:
                        exact_hits.extend(nc_hits)
                        note_matched = True
                        break
                if note_matched:
                    continue

            nlow = re.sub(r"\s*[\(\[][^)\]]*[\)\]]", "", nm).strip(" .?*!").lower()
            nlow_nopf = re.sub(
                r"^(?:starbase|pirate|enemy|hostile|conquered|gistum|gistior|gistii|gist|"
                r"pithum|pithior|pithi|pith|corelum|corelior|coreli|core|centus|centum|centior|centii|"
                r"corpus|corpum|corpior|corpii|serpentis|guristas|angel|sansha|blood raiders?|guardian angel)\s+",
                "",
                nlow,
            )
            cand_patterns: list[str | tuple[str, ...]] = []
            for key in (nlow, nlow_nopf):
                if key in STRUCTURE_ALIASES:
                    cand_patterns.extend(STRUCTURE_ALIASES[key])
            if nlow_nopf != nlow and nm_struct:
                cand_patterns.append("{fac} " + nlow_nopf)
                cand_patterns.append(nlow_nopf)
            m_ft = re.match(r"^(.+?)\s+(?:tower\s+sentry|sentry\s+tower)(?:\s+(i|ii|iii))?$", nlow, re.I)
            if m_ft:
                tier = m_ft.group(2).upper() if m_ft.group(2) else None
                tiers = [tier] if tier else ["I", "II", "III"]
                fac_word = m_ft.group(1).strip().title()
                if tier:
                    cand_patterns.append(f"Tower Sentry {fac_word} {tier}")
                else:
                    cand_patterns.append(tuple(f"Tower Sentry {fac_word} {t_rn}" for t_rn in tiers))
                cand_patterns.append(f"{fac_word} Sentry Gun")
                if tier:
                    cand_patterns.append(f"Tower Sentry {{fac}} {tier}")
                else:
                    cand_patterns.append(tuple(f"Tower Sentry {{fac}} {t_rn}" for t_rn in tiers))
            m_tws = re.match(r"^tower\s+sentry\s+([a-z]+)$", nlow, re.I)
            if m_tws:
                raw_fw = m_tws.group(1)
                fw_variants = list(dict.fromkeys([raw_fw.title(), raw_fw.rstrip("s").title()]))
                cand_patterns.append(
                    tuple(f"Tower Sentry {fw} {t_rn}" for fw in fw_variants for t_rn in ("I", "II", "III"))
                )
                cand_patterns.append(tuple(f"Tower Sentry {{fac}} {t_rn}" for t_rn in ("I", "II", "III")))
            m_en = re.match(r"^(.+?)\s+energy neutralizer (?:tower|sentry)$", nlow, re.I)
            if m_en:
                fw = m_en.group(1).strip().title()
                cand_patterns.append(tuple(f"{fw} Energy Neutralizer Sentry {t_rn}" for t_rn in ("I", "II", "III")))
                cand_patterns.append(tuple(f"{{fac}} Energy Neutralizer Sentry {t_rn}" for t_rn in ("I", "II", "III")))
            if nm_struct or cls_low in ("drone", "frigate", "cruiser", "battleship", "battlecruiser", "destroyer", "industrial"):
                cand_patterns.append("{fac} " + nm)

            inferred_nm_fac = infer_faction_from_text(nm)
            fac_pool: list[str] = []
            if inferred_nm_fac:
                fac_pool.append(inferred_nm_fac)
            for pf_fac in page_factions:
                if pf_fac not in fac_pool:
                    fac_pool.append(pf_fac)

            struct_matched: list[SdeTypeRef] = []
            # Evaluate patterns in declared priority order so {fac}-qualified faction patterns
            # run before untemplated fallbacks (never synthesizing a default faction when fac_pool is empty).
            for pat_entry in cand_patterns:
                pats = pat_entry if isinstance(pat_entry, tuple) else (pat_entry,)
                if any("{fac}" in p for p in pats):
                    if not fac_pool:
                        continue
                    for fac in fac_pool:
                        prefixes = FACTION_PREFIXES.get(fac, [fac])
                        for p in pats:
                            for pf in prefixes:
                                cand_str = p.format(fac=pf) if "{fac}" in p else p
                                h = self.direct_hits(
                                    cand_str,
                                    allow_fuzzy=False,
                                    allowed_cats=nm_cats,
                                    expected_class=cb,
                                    is_structure=nm_struct,
                                )
                                if h:
                                    struct_matched.extend(h)
                        if struct_matched:
                            break
                    if struct_matched:
                        break
                else:
                    for p in pats:
                        h = self.direct_hits(
                            p,
                            allow_fuzzy=False,
                            allowed_cats=nm_cats,
                            expected_class=cb,
                            is_structure=nm_struct,
                        )
                        if h:
                            struct_matched.extend(h)
                    if struct_matched:
                        break

            if struct_matched:
                struct_hits.extend(struct_matched)
                continue

            fh = self.find_family_candidates(nm, allowed_cats=nm_cats)
            if fh:
                fam_hits.extend(fh)
            else:
                unresolved_parts.append(nm)

        concrete_refs = exact_hits + struct_hits
        concrete_tids = sorted({h.type_id for h in concrete_refs})
        all_refs = concrete_refs + fam_hits
        all_tids = sorted({h.type_id for h in all_refs})
        matched_names = sorted({h.name for h in concrete_refs})
        name_to_type_ids_map: dict[str, list[int]] = {}
        if include_name_map and concrete_refs:
            by_nm: defaultdict[str, set[int]] = defaultdict(set)
            for h in concrete_refs:
                by_nm[h.name].add(h.type_id)
            name_to_type_ids_map = {nm: sorted(tids) for nm, tids in sorted(by_nm.items())}

        def _make_res(
            status: str,
            method: str,
            *,
            type_ids: list[int] | None = None,
            candidate_type_ids: list[int] | None = None,
            matched_list: list[str] | None = None,
            unresolved_list: list[str] | None = None,
            reason: str | None = None,
        ) -> dict[str, Any]:
            out: dict[str, Any] = {
                "status": status,
                "method": method,
                "expanded_names": names,
                "type_ids": type_ids or [],
                "candidate_type_ids": candidate_type_ids or [],
                "matched_names": matched_list or [],
                "unresolved_parts": unresolved_list or [],
                "reason": reason,
            }
            if had_ellipsis:
                out["had_ellipsis"] = True
            if include_name_map and name_to_type_ids_map and type_ids:
                out["_name_to_type_ids"] = name_to_type_ids_map
            return out

        if not unresolved_parts and concrete_tids and not fam_hits:
            if struct_hits:
                status = "resolved_structure_faction"
                method = "faction_qualified_structure"
            else:
                status = "resolved_exact"
                method = "short_form_expansion" if len(names) > 1 else "exact_or_normalized"
            return _make_res(
                status,
                method,
                type_ids=concrete_tids,
                candidate_type_ids=concrete_tids,
                matched_list=matched_names,
            )

        if not unresolved_parts and fam_hits:
            return _make_res(
                "resolved_family_candidates",
                "ellipsis_or_family_prefix" if had_ellipsis else "family_prefix_candidate",
                candidate_type_ids=all_tids,
                unresolved_list=list(names),
                reason="bare_family_prefix_only",
            )

        if concrete_tids or fam_hits:
            return _make_res(
                "partly_resolved",
                "partial_multi_name",
                type_ids=concrete_tids,
                candidate_type_ids=all_tids,
                matched_list=matched_names,
                unresolved_list=unresolved_parts,
                reason=f"unresolved_part:{';'.join(unresolved_parts)}",
            )

        # Faction + ship_class entity group hints are preserved as candidate_type_ids on unresolved rows
        # (never counted toward concrete resolved_total / resolved_percent).
        inferred_name_fac = infer_faction_from_text(raw_clean)
        nf = inferred_name_fac or (page_factions[0] if page_factions else None)
        has_role_or_fac_hint = bool(inferred_name_fac or NPC_ROLE_HINT_RE.search(raw_clean))
        if has_role_or_fac_hint and cb and nf and self.faction_class_groups.get((nf, cb)):
            cands = self.faction_class_groups[(nf, cb)]
            return _make_res(
                "resolved_class_candidates",
                f"faction_class_group:{nf}:{cb}",
                candidate_type_ids=cands,
                unresolved_list=unresolved_parts,
                reason="custom_spawn_name_mapped_via_faction_class",
            )

        return _make_res(
            "unresolved",
            "none",
            unresolved_list=unresolved_parts,
            reason="event_npc_not_in_sde" if is_event else "unmatched_name_in_sde",
        )

    def match_sde_page(
        self,
        candidate_names: list[str],
        factions: list[str],
        levels: list[int],
        part_info: dict[str, int] | None = None,
        mission_hint: str | None = None,
    ) -> dict[str, Any]:
        """Match a wiki unit/page against sde_missions, mission_dungeons, and sde_dungeons."""
        fac_ids = {CANON_FACTION_NAME_TO_ID[f] for f in factions if f in CANON_FACTION_NAME_TO_ID}
        matched_missions: dict[int, dict[str, Any]] = {}
        matched_dungeons: dict[int, dict[str, Any]] = {}

        # Prefer strict exact-title keys before expanded prefix/hyphen aliases
        for nm in candidate_names:
            strict_keys = make_title_lookup_keys(nm, strict_only=True)
            strict_m: list[dict[str, Any]] = []
            for key in strict_keys:
                strict_m.extend(self.missions_by_strict_key.get(key, []))
            m_hits = strict_m
            if not m_hits:
                for key in make_title_lookup_keys(nm, strict_only=False):
                    m_hits.extend(self.missions_by_key.get(key, []))
            for m in m_hits:
                matched_missions[m["mission_id"]] = m

            strict_d: list[dict[str, Any]] = []
            for key in strict_keys:
                strict_d.extend(self.dungeons_by_strict_key.get(key, []))
            d_hits = strict_d
            if not d_hits:
                for key in make_title_lookup_keys(nm, strict_only=False):
                    d_hits.extend(self.dungeons_by_key.get(key, []))
            for d in d_hits:
                matched_dungeons[d["dungeon_id"]] = d

        # Narrow mission candidates before linking mission_dungeons so L1-L4 dungeons stay separated
        mission_list = list(matched_missions.values())
        if len(mission_list) > 1:
            exact_lows = {norm_title(nm).lower() for nm in candidate_names if nm}
            exact_m = [m for m in mission_list if (m.get("name") or "").strip().lower() in exact_lows]
            if exact_m:
                mission_list = exact_m
        if len(mission_list) > 1 and mission_hint:
            if mission_hint == "courier":
                m_c = [m for m in mission_list if m.get("is_courier")]
                if m_c:
                    mission_list = m_c
            elif mission_hint == "non_courier":
                m_nc = [m for m in mission_list if not m.get("is_courier")]
                if m_nc:
                    mission_list = m_nc
        if len(mission_list) > 1 and fac_ids:
            m_fac = [
                m for m in mission_list
                if (did := self.mission_dungeons.get(m["mission_id"])) is not None
                and (ld := self.dungeons_by_id.get(did)) is not None
                and ld.get("faction_id") in fac_ids
            ]
            if m_fac:
                mission_list = m_fac
        if len(mission_list) > 1 and levels:
            m_lvl = [
                m for m in mission_list
                if (did := self.mission_dungeons.get(m["mission_id"])) is not None
                and (ld := self.dungeons_by_id.get(did)) is not None
                and ld.get("difficulty") in levels
            ]
            if m_lvl:
                mission_list = m_lvl

        # Courier/distribution missions without mission_dungeons links must not pull unrelated same-named dungeons
        if mission_hint == "courier" and mission_list and all(m.get("is_courier") for m in mission_list):
            matched_dungeons.clear()

        # Link mission_dungeons from the narrowed mission_list into matched_dungeons
        for m in mission_list:
            mid = m["mission_id"]
            if mid in self.mission_dungeons:
                did = self.mission_dungeons[mid]
                if did in matched_dungeons:
                    # Shallow-copy before tagging so self.dungeons_by_id is never mutated across pages
                    matched_dungeons[did] = {**matched_dungeons[did], "linked_via_mission_ext": True}
                else:
                    real_d = self.dungeons_by_id.get(did)
                    if real_d is not None:
                        matched_dungeons[did] = {**real_d, "linked_via_mission_ext": True}
                    else:
                        matched_dungeons[did] = {
                            "dungeon_id": did,
                            "name": m["name"],
                            "faction_id": None,
                            "difficulty": None,
                            "linked_via_mission_ext": True,
                        }

        # Narrow dungeon candidates when multiple factions/difficulties/parts exist and page specifies them
        dungeon_list = list(matched_dungeons.values())
        if len(dungeon_list) > 1:
            exact_lows = {norm_title(nm).lower() for nm in candidate_names if nm}
            exact_d = [d for d in dungeon_list if (d.get("name") or "").strip().lower() in exact_lows]
            if exact_d and not (part_info and len(dungeon_list) == part_info.get("of")):
                dungeon_list = exact_d
        if len(dungeon_list) > 1 and fac_ids:
            fac_filtered = [d for d in dungeon_list if d.get("faction_id") in fac_ids]
            if fac_filtered:
                dungeon_list = fac_filtered
        if len(dungeon_list) > 1 and levels:
            lvl_filtered = [d for d in dungeon_list if d.get("difficulty") in levels]
            if lvl_filtered:
                dungeon_list = lvl_filtered
            else:
                # Avoid cross-level duplication when explicit level is absent in SDE (e.g. L2 wiki matched L1 minmatar)
                has_explicit_diff = all(d.get("difficulty") is not None for d in dungeon_list)
                linked_any = any(d.get("linked_via_mission_ext") for d in dungeon_list)
                if has_explicit_diff and not linked_any and any(d.get("difficulty") not in levels for d in dungeon_list):
                    dungeon_list = []

        # Multi-stage expedition dungeons with identical name/faction/difficulty map 1-to-1 by ascending dungeon_id
        if part_info and len(dungeon_list) > 1:
            p_num = part_info.get("part", 0)
            p_total = part_info.get("of", 0)
            if p_total and 1 <= p_num <= p_total and len(dungeon_list) == p_total:
                sigs = {
                    (
                        norm_title(d.get("name") or "").lower(),
                        d.get("faction_id"),
                        d.get("difficulty"),
                    )
                    for d in dungeon_list
                }
                if len(sigs) == 1:
                    ordered = sorted(dungeon_list, key=lambda d: d["dungeon_id"])
                    dungeon_list = [ordered[p_num - 1]]

        mission_ids = sorted({m["mission_id"] for m in mission_list})
        dungeon_ids = sorted({d["dungeon_id"] for d in dungeon_list})
        m_by_id = {m["mission_id"]: m for m in mission_list}
        return {
            "mission_ids": mission_ids,
            "dungeon_ids": dungeon_ids,
            "missions": [m_by_id[mid] for mid in mission_ids],
            "dungeons": sorted(dungeon_list, key=lambda d: d["dungeon_id"]),
        }


_STAGE_ORDINAL_WORDS: dict[str, int] = {
    "first": 1, "1st": 1, "one": 1, "1": 1,
    "second": 2, "2nd": 2, "two": 2, "2": 2,
    "third": 3, "3rd": 3, "three": 3, "3": 3,
    "fourth": 4, "4th": 4, "four": 4, "4": 4,
    "fifth": 5, "5th": 5, "five": 5, "5": 5,
}


def _parse_stage_heading(txt: str) -> tuple[int, int | None] | None:
    """Parse stage index (and optional total) from headings like 'First Location', '1st location', or 'Part 2 of 3'."""
    cleaned = txt.strip()
    m_part = re.match(r"^(?:part|stage)\s*#?(\d+)(?:\s+of\s+(\d+))?\b", cleaned, re.I)
    if m_part:
        return int(m_part.group(1)), (int(m_part.group(2)) if m_part.group(2) else None)
    m_loc1 = re.match(r"^location\s+#?(\d+|one|two|three|four|five)\b", cleaned, re.I)
    if m_loc1:
        tok = m_loc1.group(1).lower()
        return (max(1, int(tok)) if tok.isdigit() else _STAGE_ORDINAL_WORDS[tok]), None
    m_loc2 = re.match(
        r"^(first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th)(?:\s+and\s+last)?\s+location\b",
        cleaned,
        re.I,
    )
    if m_loc2:
        return _STAGE_ORDINAL_WORDS[m_loc2.group(1).lower()], None
    return None


def parse_wiki_page(
    title: str,
    categories: list[str],
    wikitext: str,
    sde: SdeIndex,
) -> dict[str, Any] | None:
    """Parse a single MediaWiki PvE article into structured units, rooms, waves, and stats."""
    events = iter_templates_and_headings(wikitext)
    has_pve_template = any(
        e["kind"] == "template"
        and e["name"] in (
            "missiondetails",
            "cmbsiteinfo",
            "cmbsite",
            "cmbsitebox",
            "npctablerow",
            "npctr",
            "npctablehead",
            "npcth",
        )
        for e in events
    )
    if not has_pve_template:
        return None

    tinfo = parse_title_qualifiers(title)
    is_event = "Game events" in categories
    is_past = "Past content" in categories
    prose_escalation_links = extract_forward_escalation_links(wikitext, title)

    # Build raw units grouped around Missiondetails / CMBSite* headers (splitting variants and stages)
    units_raw: list[dict[str, Any]] = []
    cur_heading_stack: list[tuple[int, str]] = []
    cur_unit: dict[str, Any] | None = None
    preamble_events: list[dict[str, Any]] = []
    variant_heading_level: int | None = None
    stage_heading_level: int | None = None

    def current_heading_text() -> str:
        return " > ".join(h for _, h in cur_heading_stack)

    def unit_has_npc_rows(u: dict[str, Any]) -> bool:
        return any(
            e["kind"] == "template" and e["name"] in ("npctablerow", "npctr")
            for e in u["events"]
        )

    def lookahead_has_npcs(start_idx: int, stop_level: int) -> bool:
        for nxt in events[start_idx + 1 :]:
            if nxt["kind"] == "heading" and nxt["level"] <= stop_level:
                break
            if nxt["kind"] == "template" and nxt["name"] in ("npctablerow", "npctr"):
                return True
        return False

    _SITE_CATS = {
        "Combat Anomalies",
        "DED Complexes",
        "Unrated Complexes",
        "Expeditions",
        "COSMOS sites",
        "Relic sites",
        "Data sites",
        "Gas sites",
        "Ore sites",
        "Wormhole sites",
        "Chemical Labs",
    }

    def infer_default_unit_kind() -> str:
        return "combat_site" if set(categories) & _SITE_CATS else "mission"

    def lookahead_has_unit_template(start_idx: int, stop_level: int) -> bool:
        for nxt in events[start_idx + 1 :]:
            if nxt["kind"] == "heading" and nxt["level"] <= stop_level:
                break
            if nxt["kind"] == "template" and nxt["name"] in ("missiondetails", "cmbsiteinfo", "cmbsite", "cmbsitebox"):
                return True
        return False

    for idx, ev in enumerate(events):
        if ev["kind"] == "heading":
            lvl = ev["level"]
            txt = ev["text"]
            cur_heading_stack = [(l, h) for l, h in cur_heading_stack if l < lvl]
            cur_heading_stack.append((lvl, txt))

            in_non_variant_sec = any(NON_VARIANT_SECTION_RE.search(h) for _, h in cur_heading_stack)
            if in_non_variant_sec and cur_unit is not None:
                if variant_heading_level is not None and lvl <= variant_heading_level:
                    variant_heading_level = None
                if stage_heading_level is not None and lvl <= stage_heading_level:
                    stage_heading_level = None
                cur_unit["events"].append(ev)
                continue

            # Split multi-stage expedition/mission headings (e.g. 'First Location'..'Fourth Location', 'Part 1'..'Part 4')
            stage_hit = _parse_stage_heading(txt) if STAGE_HEADING_RE.match(txt) else None
            if (
                stage_hit is not None
                and not in_non_variant_sec
                and not lookahead_has_unit_template(idx, lvl)
                and lookahead_has_npcs(idx, lvl)
                and (stage_heading_level is None or lvl == stage_heading_level)
            ):
                p_num, p_total = stage_hit
                variant_heading_level = None
                p_info = {"part": p_num, "of": p_total} if p_total else {"part": p_num, "of": None}
                if cur_unit is None:
                    stage_heading_level = lvl
                    cur_unit = {
                        "unit_kind": infer_default_unit_kind(),
                        "header": None,
                        "section_heading": txt,
                        "variant_name": None,
                        "stage_heading": txt,
                        "stage_part_info": p_info,
                        "events": [],
                        "escalation_links": [],
                        "header_group_id": -1,
                    }
                    preamble_events.clear()
                    units_raw.append(cur_unit)
                elif not unit_has_npc_rows(cur_unit):
                    cur_unit["section_heading"] = txt
                    cur_unit["stage_heading"] = txt
                    cur_unit["stage_part_info"] = p_info
                    cur_unit["events"] = [
                        e for e in cur_unit["events"] if e["kind"] != "heading" or e["level"] < lvl
                    ]
                    stage_heading_level = lvl
                else:
                    stage_heading_level = lvl
                    cur_unit = {
                        "unit_kind": cur_unit["unit_kind"],
                        "header": cur_unit["header"],
                        "section_heading": txt,
                        "variant_name": None,
                        "stage_heading": txt,
                        "stage_part_info": p_info,
                        "events": [],
                        "escalation_links": [],
                        "header_group_id": cur_unit["header_group_id"],
                    }
                    units_raw.append(cur_unit)
                continue

            if cur_unit is not None and cur_unit["unit_kind"] == "combat_site":
                is_room_h = bool(ROOM_HEADING_RE.search(txt))
                is_explicit_variant = (not is_room_h) and bool(VARIANT_HEADING_RE.search(txt))
                is_sibling_variant = (
                    (not is_room_h)
                    and variant_heading_level is not None
                    and lvl == variant_heading_level
                )
                has_future_variant_sibling = False
                if (not is_room_h) and not is_explicit_variant:
                    for nxt in events[idx + 1 :]:
                        if nxt["kind"] == "template" and nxt["name"] in (
                            "missiondetails",
                            "cmbsiteinfo",
                            "cmbsite",
                            "cmbsitebox",
                        ):
                            break
                        if nxt["kind"] == "heading":
                            if nxt["level"] < lvl:
                                break
                            if (
                                nxt["level"] == lvl
                                and not NON_VARIANT_SECTION_RE.search(nxt["text"])
                                and VARIANT_HEADING_RE.search(nxt["text"])
                            ):
                                has_future_variant_sibling = True
                                break
                if (is_explicit_variant or is_sibling_variant or has_future_variant_sibling) and lookahead_has_npcs(idx, lvl):
                    if not unit_has_npc_rows(cur_unit):
                        cur_unit["variant_name"] = txt
                        cur_unit["events"] = [
                            e for e in cur_unit["events"] if e["kind"] != "heading" or e["level"] < lvl
                        ]
                        variant_heading_level = lvl
                    else:
                        variant_heading_level = lvl
                        new_unit: dict[str, Any] = {
                            "unit_kind": "combat_site",
                            "header": cur_unit["header"],
                            "section_heading": cur_unit["section_heading"],
                            "variant_name": txt,
                            "events": [],
                            "escalation_links": list(cur_unit["escalation_links"]),
                            "header_group_id": cur_unit["header_group_id"],
                        }
                        if cur_unit.get("stage_heading") is not None:
                            new_unit["stage_heading"] = cur_unit["stage_heading"]
                        if cur_unit.get("stage_part_info") is not None:
                            new_unit["stage_part_info"] = dict(cur_unit["stage_part_info"])
                        cur_unit = new_unit
                        units_raw.append(cur_unit)
                    continue
                if variant_heading_level is not None and lvl <= variant_heading_level:
                    variant_heading_level = None

            if cur_unit is not None:
                cur_unit["events"].append(ev)
            else:
                preamble_events.append(ev)
            continue

        tname = ev["name"]
        if tname == "missiondetails":
            variant_heading_level = None
            stage_heading_level = None
            cur_unit = {
                "unit_kind": "mission",
                "header": ev,
                "section_heading": current_heading_text(),
                "variant_name": None,
                "events": list(preamble_events) if not units_raw and preamble_events else [],
                "escalation_links": [],
                "header_group_id": idx,
            }
            preamble_events.clear()
            units_raw.append(cur_unit)
        elif tname in ("cmbsiteinfo", "cmbsite", "cmbsitebox"):
            # Escalation teaser box without NPC rows inside an Escalation/Expedition section must not spawn a phantom unit
            if units_raw:
                in_esc_section = any(re.search(r"\b(?:escalation|expedition)\b", h, re.I) for _, h in cur_heading_stack)
                has_following_rows = False
                for nxt in events[idx + 1 :]:
                    if nxt["kind"] == "template" and nxt["name"] in ("missiondetails", "cmbsiteinfo", "cmbsite", "cmbsitebox"):
                        break
                    if nxt["kind"] == "template" and nxt["name"] in ("npctablerow", "npctr"):
                        has_following_rows = True
                        break
                if in_esc_section and not has_following_rows:
                    link_nm = clean_wiki_inline(ev["kv"].get("name") or (ev["pos_args"][0] if ev["pos_args"] else ""))
                    if link_nm:
                        tail = units_raw[-1]
                        tail_key = (tail.get("header_group_id"), tail.get("stage_heading"))
                        for u in units_raw:
                            if (u.get("header_group_id"), u.get("stage_heading")) == tail_key:
                                if link_nm not in u["escalation_links"]:
                                    u["escalation_links"].append(link_nm)
                    continue
            variant_heading_level = None
            stage_heading_level = None
            cur_unit = {
                "unit_kind": "combat_site",
                "header": ev,
                "section_heading": current_heading_text(),
                "variant_name": None,
                "events": list(preamble_events) if not units_raw and preamble_events else [],
                "escalation_links": [],
                "header_group_id": idx,
            }
            preamble_events.clear()
            units_raw.append(cur_unit)
        else:
            if cur_unit is not None:
                cur_unit["events"].append(ev)
            else:
                preamble_events.append(ev)

    if not units_raw:
        units_raw.append(
            {
                "unit_kind": infer_default_unit_kind(),
                "header": None,
                "section_heading": "",
                "variant_name": None,
                "events": preamble_events,
                "escalation_links": [],
                "header_group_id": -1,
            }
        )

    # Finalize multi-stage totals per header group (e.g. Location 1..4 -> of=4)
    stage_groups: dict[int, list[dict[str, Any]]] = defaultdict(list)
    for uraw in units_raw:
        if uraw.get("stage_part_info"):
            stage_groups[uraw.get("header_group_id", -1)].append(uraw)
    for s_list in stage_groups.values():
        distinct_parts = {u["stage_part_info"]["part"] for u in s_list if u["stage_part_info"].get("part")}
        inferred_of = max(
            [u["stage_part_info"].get("of") or 0 for u in s_list]
            + list(distinct_parts)
            + [len(distinct_parts)]
        )
        for u in s_list:
            if u["stage_part_info"].get("of") is None:
                u["stage_part_info"]["of"] = inferred_of

    # Attach page-level forward prose escalation links to non-stage units or all final-stage variants
    if prose_escalation_links and units_raw:
        stage_tail = [u for u in units_raw if u.get("stage_part_info")]
        if len(stage_tail) == len(units_raw):
            max_part = max(int(u["stage_part_info"]["part"]) for u in stage_tail)
            targets = [u for u in stage_tail if int(u["stage_part_info"]["part"]) == max_part]
        else:
            targets = units_raw
        for u in targets:
            for lk in prose_escalation_links:
                if lk not in u["escalation_links"]:
                    u["escalation_links"].append(lk)

    # Count sibling variants sharing the same header (scoped per stage when multi-stage)
    variant_groups: dict[tuple[int, str | None], list[dict[str, Any]]] = defaultdict(list)
    for uraw in units_raw:
        if uraw.get("variant_name"):
            variant_groups[(id(uraw["header"]), uraw.get("stage_heading"))].append(uraw)
    for group_list in variant_groups.values():
        if len(group_list) > 1:
            for v_idx, uraw in enumerate(group_list, start=1):
                uraw["variant_index"] = v_idx
                uraw["variant_count"] = len(group_list)

    parsed_units: list[dict[str, Any]] = []
    for u_idx, uraw in enumerate(units_raw, start=1):
        unit = _finalize_unit(
            page_title=title,
            tinfo=tinfo,
            categories=categories,
            is_event=is_event,
            is_past=is_past,
            unit_index=u_idx,
            uraw=uraw,
            sde=sde,
        )
        parsed_units.append(unit)

    return {
        "page_title": title,
        "base_title": tinfo["base_title"],
        "title_qualifiers": tinfo["qualifiers"],
        "categories": sorted(categories),
        "is_event": is_event,
        "is_past_content": is_past,
        "units": parsed_units,
    }


_INITIAL_HEAD_RE = re.compile(
    r"\b(?:initial|warp-?in|first\s+spawn|starting|on\s+grid|at\s+arrival|on\s+arrival)\b",
    re.I,
)
_INITIAL_WAVE_SPAWN_RE = re.compile(
    r"\b(?:initial|warp-?in|first\s+spawn|starting|defenders|on\s+grid|at\s+arrival|on\s+arrival|upon\s+arrival)\b",
    re.I,
)
_STATIC_STRUCT_SPATIAL_RE = re.compile(
    r"\b(?:(?:near|from|at|by|around|surrounding|camping|towards?|behind|beside|"
    r"approach(?:ing)?|attack(?:ing)?|destroy(?:ing)?|next\s+to|close\s+to|triggered\s+by|"
    r"guarded\s+by|guarding|protecting)\s+(?:(?:the|an?|this|that)\s+)?(?:[\w'-]+\s+){0,2}|"
    r"(?:\d+\s*%\s*))"
    r"(?:structures?|outposts?|battlestations?|stations?|gates?|acceleration\s+gates?|"
    r"bunkers?|towers?|control\s+towers?|sentr(?:y|ies)|asteroids?|objects?|containers?|"
    r"crates?|mines?|beacons?)\b",
    re.I,
)
_STATIC_STRUCT_NOUN_RE = re.compile(
    r"\b(?:structures?|gates?|bunkers?|towers?|sentr(?:y|ies)|asteroids?|objects?|"
    r"containers?|crates?|hacking\s+containers?|archaeology|mines?)\b",
    re.I,
)
_COMBAT_SHIP_CLASS_RE = re.compile(
    r"\b(?:frigate|ff|destroyer|dd|cruiser|battlecruiser|bc|battleship|bs|carrier|supercarrier|dreadnought|titan|commander)\b",
    re.I,
)
_INLINE_TRIGGER_CLAUSE_RE = re.compile(
    r"\btriggers\s+(?:the\s+)?(?:next\s+)?(?:[\w-]+\s+){0,2}?(?:waves?|reinforcements?|spawns?)\b(?:\s*#?\s*\d+)?",
    re.I,
)
_SEQ_SEPARATOR_RE = re.compile(
    r"\b(?:(?:waves?|reinforce(?:ments?)?|timed)\b|"
    r"triggered\s+(?:by|when)\b|"
    r"next\s+(?:waves?|reinforcements?|spawns?)\b|"
    r"(?:first|second|third|fourth|fifth|\d+(?:st|nd|rd|th))\s+(?:waves?|reinforcements?|spawns?)\b|"
    r"spawns?\b(?!\s+(?:(?:at|near|around|from|by)\b|"
    r"in\s+(?!the\s+next\b)(?!(?:waves?|reinforcements?|\d+\s*(?:mins?|minutes?|secs?|seconds?|hrs?|hours?))\b)|"
    r"as\s+\d+\s+groups?\b))|"
    r"\d+\s*%)",
    re.I,
)


def _strip_non_trigger_parens(label: str) -> str:
    """Remove parentheticals whose clauses are solely non-trigger notes (outgoing/spatial/aggro)."""
    def _repl(m: re.Match[str]) -> str:
        inner = m.group(1).strip()
        clauses = [seg.strip() for seg in re.split(r"[,;]+", inner) if seg.strip()]
        if clauses and all(_is_non_trigger_header_clause(c) for c in clauses):
            return " "
        return m.group(0)

    return re.sub(r"\(([^()]+)\)", _repl, label)


def _is_sequential_separator_label(label: str) -> bool:
    """Return True if a separator label names a distinct sequential wave rather than a simultaneous sub-group."""
    htrig = classify_wave_header_trigger(label)
    if htrig is not None and htrig.get("classification_state") != "unclassified":
        return True
    probe = _strip_non_trigger_parens(label)
    probe = _INLINE_TRIGGER_CLAUSE_RE.sub(" ", probe)
    return bool(_SEQ_SEPARATOR_RE.search(probe))


_WORD_ORDINALS: dict[str, int] = {
    "first": 1, "1st": 1, "one": 1,
    "second": 2, "2nd": 2, "two": 2,
    "third": 3, "3rd": 3, "three": 3,
    "fourth": 4, "4th": 4, "four": 4,
    "fifth": 5, "5th": 5, "five": 5,
    "sixth": 6, "6th": 6, "six": 6,
    "seventh": 7, "7th": 7, "seven": 7,
    "eighth": 8, "8th": 8, "eight": 8,
}

_INITIAL_WAVE_TIMINGS = ("initial_spawn", "simultaneous_initial_group")

_ORDINAL_BEFORE_WAVE_RE = re.compile(
    r"\b(?:(?:(\d+)(?:st|nd|rd|th)|(first|second|third|fourth|fifth|sixth|seventh|eighth))\s+"
    r"(waves?|reinforcements?|spawns?|groups?|batter(?:y|ies)|fleets?)|"
    r"(\d+)\s+(wave|reinforcement|spawn|group|battery|fleet)\b"
    r"(?!\s+(?:waves|reinforcements|spawns|groups|batteries|fleets|ships|frigates|destroyers|cruisers|battlecruisers|battleships|towers|sentries|\d+)\b))",
    re.I,
)
_KW_NUM_BEFORE_WAVE_RE = re.compile(
    r"\b(waves?|reinforcements?|spawn|groups?|fleets?|batteries|battery)\s*#?\s*(\d+)\b"
    r"(?!\s+(?:km|m|au|seconds?|secs?|mins?|minutes?|hrs?|hours?|%)\b)"
    r"|\b(spawns)\s*#\s*(\d+)\b",
    re.I,
)
_NAMED_WAVE_RE = re.compile(
    r"\b(?:the\s+)?((?:assault\s+)?(?:frigate|destroyer|cruiser|battlecruiser|battleship)|"
    r"commander|boss|drone|smartbomb|fighter|interceptor|battery|sentry|relay|station|backup)\s+"
    r"(wave|spawn|reinforcement)\b(?:\s*#?\s*(\d+)\b)?",
    re.I,
)
_TRIGGER_SHIP_STOPWORDS = frozenset(
    {
        "death", "killed", "kill", "destroyed", "destroy", "destruction",
        "attack", "attacked", "aggress", "aggressed", "damage", "damaged",
        "shield", "shields", "armor", "hull", "structure", "structures",
        "trigger", "triggers", "triggered", "spawn", "spawns", "spawned",
        "wave", "waves", "reinforcement", "reinforcements", "group", "groups",
        "fleet", "fleets", "battery", "batteries", "commander", "boss",
        "frigate", "frigates", "destroyer", "destroyers", "cruiser", "cruisers",
        "battlecruiser", "battlecruisers", "battleship", "battleships",
        "industrial", "drone", "drones", "sentry", "sentries", "tower", "towers",
        "first", "second", "third", "fourth", "last", "next", "final",
        "initial", "backup", "assault", "ship", "ships", "class", "type",
        "pirate", "pirates", "angel", "guristas", "serpentis", "sansha",
        "blood", "mercenary", "mercenaries", "rogue",
        "the", "and", "for", "all", "are", "not", "may", "can", "you",
        "off", "out", "has", "was", "get", "its", "one", "two", "six",
        "ten", "iii", "bay", "red", "low", "top",
    }
)
_TRIGGER_TARGET_STRIP_RE = re.compile(
    r"\b(?:triggers?|spawns?)\s+(?:for\s+)?(?:the\s+)?"
    r"(?:(?:first|second|third|fourth|fifth|sixth|seventh|eighth|\d+(?:st|nd|rd|th)|"
    r"next|last|final|another|more|additional|assault|frigate|destroyer|cruiser|battlecruiser|"
    r"battleship|commander|boss|drone|smartbomb|fighter|interceptor|battery|sentry|relay|"
    r"station|backup)\s+)*(?:waves?|reinforcements?|spawns?|groups?|fleets?|batter(?:y|ies))\b"
    r"(?:\s*#?\s*\d+(?:\s*(?:,|/|&|\band\b)\s*#?\s*\d+)*)?",
    re.I,
)


def _is_targetable_forward_wave(cand_w: dict[str, Any], src_w: dict[str, Any]) -> bool:
    """Return True iff cand_w is strictly later, sequential/optional combat, and not a same-phase sibling of src_w."""
    if cand_w["wave_index"] <= src_w["wave_index"]:
        return False
    if cand_w["timing"] in (*_INITIAL_WAVE_TIMINGS, "static_structures"):
        return False
    src_phase = src_w.get("parent_phase")
    if src_phase and cand_w.get("parent_phase") == src_phase:
        return False
    return True


def _match_wave_targets_with_provenance(
    text: str,
    src_wave_idx: int,
    waves: list[dict[str, Any]],
) -> list[tuple[int, str | None]]:
    """Resolve referenced forward target waves with optional inference_rule for positional offsets."""
    low = text.lower()
    if not waves:
        return []
    src_wave = waves[src_wave_idx - 1] if 1 <= src_wave_idx <= len(waves) else {"wave_index": src_wave_idx}
    earlier_waves = [w for w in waves if w["wave_index"] <= src_wave_idx and w["timing"] != "static_structures"]
    targetable_later_waves = [w for w in waves if _is_targetable_forward_wave(w, src_wave)]
    targetable_idxs = {w["wave_index"] for w in targetable_later_waves}
    combat_seq = [w["wave_index"] for w in waves if w["timing"] != "static_structures"]
    init_pos = max(
        (i for i, wi in enumerate(combat_seq) if waves[wi - 1]["timing"] in _INITIAL_WAVE_TIMINGS),
        default=0,
    )

    def _kw_pat(kw: str) -> str:
        return "batter(?:y|ies)" if kw == "battery" else rf"{kw}s?"

    def _wave_matches_label(w: dict[str, Any], kind_words: tuple[str, ...], num: int) -> bool:
        ord_words = [k for k, v in _WORD_ORDINALS.items() if v == num]
        wl = (w.get("group_name") or w["wave_name"]).lower()
        full_wl = w["wave_name"].lower()
        for txt in (wl, full_wl):
            for kw in kind_words:
                kp = _kw_pat(kw)
                if re.search(rf"\b{kp}\s*#?\s*{num}\b", txt):
                    return True
                for ow in ord_words:
                    if re.search(rf"\b{ow}\s+{kp}\b", txt):
                        return True
        return False

    def _find_by_label_num(kind_words: tuple[str, ...], num: int) -> int | None:
        for kw in dict.fromkeys(kind_words):
            for w in targetable_later_waves:
                if _wave_matches_label(w, (kw,), num):
                    return w["wave_index"]
        return None

    def _has_earlier_label(kind_word: str, num: int) -> bool:
        return any(_wave_matches_label(w, (kind_word,), num) for w in earlier_waves)

    def _resolve_num_target(kw_root: str, num: int, kind_words: tuple[str, ...]) -> tuple[int | None, str | None]:
        w_hit = _find_by_label_num(kind_words, num)
        if w_hit is not None:
            return w_hit, None
        if kw_root == "wave" and num >= 1 and not _has_earlier_label("wave", num):
            pos = num - 1
            if 0 <= pos < len(combat_seq):
                cand = combat_seq[pos]
                if cand in targetable_idxs:
                    return cand, "positional_wave_index"
        if kw_root in ("reinforcement", "spawn", "fleet", "battery") and not _has_earlier_label(kw_root, num):
            pos = init_pos + num
            if 0 <= pos < len(combat_seq):
                cand = combat_seq[pos]
                if cand in targetable_idxs:
                    return cand, "positional_reinforcement_offset"
        return None, None

    def _norm_kw(raw_kw: str) -> str:
        kw_root = raw_kw.rstrip("s")
        return "battery" if kw_root == "batterie" else kw_root

    def _find_named_wave_target(keyw: str, noun: str, num_tok: str | None) -> int | None:
        phrase = f"{keyw} {noun} {num_tok}" if num_tok else f"{keyw} {noun}"
        for w in targetable_later_waves:
            for txt in ((w.get("group_name") or "").lower(), w["wave_name"].lower()):
                if not txt:
                    continue
                if re.search(rf"(?:^|\b){re.escape(phrase)}\b", txt) and not (
                    keyw in ("frigate", "cruiser") and re.search(rf"\bassault\s+{re.escape(phrase)}\b", txt)
                ):
                    return w["wave_index"]
        if num_tok:
            return None
        kw_pat = "batter(?:y|ies)" if keyw == "battery" else rf"{re.escape(keyw)}(?:s|es)?"
        for w in targetable_later_waves:
            for txt in ((w.get("group_name") or "").lower(), w["wave_name"].lower()):
                if not txt:
                    continue
                if re.search(rf"\b{kw_pat}\b", txt) and not (
                    keyw in ("frigate", "cruiser") and re.search(rf"\bassault\s+{kw_pat}\b", txt)
                ):
                    return w["wave_index"]
        return None

    targets: list[tuple[int, str | None]] = []
    seen_idxs: set[int] = set()

    def _add_hit(w_hit: int | None, rule: str | None) -> None:
        if w_hit is not None and w_hit not in seen_idxs:
            seen_idxs.add(w_hit)
            targets.append((w_hit, rule))

    for m_named in _NAMED_WAVE_RE.finditer(low):
        if m_named.group(3):
            _add_hit(_find_named_wave_target(m_named.group(1), m_named.group(2), m_named.group(3)), None)

    m_multi = re.finditer(
        r"\b(waves?|reinforcements?|spawns?|groups?|fleets?|batteries|battery)\s*#?\s*"
        r"(\d+)\s*(?:,|/|&|\band\b)\s*#?\s*(\d+)(?:\s*(?:,|&|\band\b)\s*#?\s*(\d+))?",
        low,
    )
    for m in m_multi:
        kw_root = _norm_kw(m.group(1))
        kind_words = (kw_root, "wave", "reinforcement", "spawn", "group")
        for g_idx in (2, 3, 4):
            tok = m.group(g_idx)
            if tok:
                _add_hit(*_resolve_num_target(kw_root, int(tok), kind_words))

    if not targets:
        for m in _ORDINAL_BEFORE_WAVE_RE.finditer(low):
            if m.group(4):
                num = int(m.group(4))
                kw = _norm_kw(m.group(5))
            else:
                num = int(m.group(1)) if m.group(1) else _WORD_ORDINALS[m.group(2)]
                kw = _norm_kw(m.group(3))
            _add_hit(*_resolve_num_target(kw, num, (kw, "wave", "reinforcement", "spawn", "group")))
        for m in _KW_NUM_BEFORE_WAVE_RE.finditer(low):
            kw = _norm_kw(m.group(1) or m.group(3))
            num = int(m.group(2) or m.group(4))
            _add_hit(*_resolve_num_target(kw, num, (kw, "wave", "reinforcement", "spawn", "group")))

    if not targets:
        for m_named in _NAMED_WAVE_RE.finditer(low):
            _add_hit(_find_named_wave_target(m_named.group(1), m_named.group(2), m_named.group(3)), None)

    return targets


def _match_wave_targets_from_text(
    text: str,
    src_wave_idx: int,
    waves: list[dict[str, Any]],
) -> list[int]:
    """Resolve referenced target wave_index values (1-based) strictly forward within a room."""
    return [idx for idx, _ in _match_wave_targets_with_provenance(text, src_wave_idx, waves)]


def _identify_trigger_ship(
    spawn: dict[str, Any],
    orig_text: str,
    room_idx: int,
    wave_idx: int,
    spawn_idx: int,
) -> dict[str, Any]:
    """Identify the symbolic trigger ship tag, narrowed ship name, and exact SDE type_ids on a spawn."""
    tag = f"r{room_idx}_w{wave_idx}_s{spawn_idx}"
    res = spawn["resolution"]
    matched_names: list[str] = res.get("matched_names") or []
    type_ids: list[int] = res.get("type_ids") or []
    name_to_tids: dict[str, list[int]] = res.get("_name_to_type_ids") or {}
    expanded: list[str] = res.get("expanded_names") or []
    out: dict[str, Any] = {"trigger_ship_tag": tag}

    subject_trig = _TRIGGER_TARGET_STRIP_RE.sub(" ", orig_text.lower())
    narrowed_names: list[str] = []
    narrowed_ids: list[int] = []
    pool_names = matched_names or expanded or ([spawn["raw_name"]] if spawn["raw_name"] else [])
    if len(pool_names) > 1 and subject_trig.strip():
        scored_hits: list[tuple[int, str]] = []
        for nm in pool_names:
            tokens = [
                t.lower()
                for t in re.findall(r"[A-Za-z0-9'-]+", nm)
                if len(t) >= 3 and t.lower() not in _TRIGGER_SHIP_STOPWORDS
            ]
            # Score against all discriminating name tokens so shared rank words (Chief/Leader/Admiral) never eclipse a specific match
            score = sum(1 for tok in set(tokens) if re.search(rf"\b{re.escape(tok)}\b", subject_trig))
            if score > 0:
                scored_hits.append((score, nm))
        if scored_hits:
            best_score = max(score for score, _ in scored_hits)
            top_names = [nm for score, nm in scored_hits if score == best_score]
            if 0 < len(top_names) < len(pool_names):
                narrowed_names = top_names
                for nm in narrowed_names:
                    if nm in name_to_tids:
                        narrowed_ids.extend(name_to_tids[nm])

    if narrowed_names:
        out["trigger_ship_name"] = narrowed_names[0]
        if narrowed_ids:
            out["trigger_type_ids"] = sorted(set(narrowed_ids))
    elif len(pool_names) == 1 and pool_names[0]:
        out["trigger_ship_name"] = pool_names[0]
        if name_to_tids.get(pool_names[0]):
            out["trigger_type_ids"] = list(name_to_tids[pool_names[0]])
        elif type_ids:
            out["trigger_type_ids"] = list(type_ids)
    elif len(type_ids) == 1:
        if pool_names:
            out["trigger_ship_name"] = pool_names[0]
        out["trigger_type_ids"] = list(type_ids)
    elif type_ids and (
        "/" in (spawn.get("raw_name") or "")
        or "," in (spawn.get("raw_name") or "")
        or len(expanded) > 1
        or res.get("status") == "resolved_structure_faction"
        or _is_structure_row(spawn.get("raw_class") or "", spawn.get("raw_name") or "")
    ):
        tname = spawn.get("raw_name") or (pool_names[0] if pool_names else "")
        if tname:
            out["trigger_ship_name"] = tname
        out["trigger_type_ids"] = list(type_ids)
    return out


def _refresh_wave_link(w: dict[str, Any], inference_rule_override: str | None = None) -> None:
    """Recompute wave_link from wave timing and wave_trigger while preserving custom inference_rule."""
    is_sep = bool(w.get("group_name"))
    wl = _build_wave_link(w["wave_name"], w["timing"], w.get("wave_trigger"), is_separator=is_sep)
    if inference_rule_override and wl["classification_state"] == "inferred":
        wl["inference_rule"] = inference_rule_override
    w["wave_link"] = wl


def wire_room_waves_and_triggers(rooms: list[dict[str, Any]]) -> None:
    """Resolve initial anchors, target_wave_index, trigger_ship_tag, and wave_trigger links without default guessing."""
    for r in rooms:
        r_idx = r["room_index"]
        waves = r["waves"]
        # Step 1: Promote untriggered first combat wave (and contiguous same-phase siblings)
        # only when it unambiguously starts the room (never promoting Wave 2+ / delayed / reinforcement).
        for w in waves:
            t = w["timing"]
            if t == "static_structures":
                continue
            wlow = (w.get("group_name") or w["wave_name"]).lower()
            promoted_anchor = False
            was_initial = t in _INITIAL_WAVE_TIMINGS
            if was_initial:
                promoted_anchor = True
            elif (
                w.get("wave_trigger") is None
                and t != "optional_spawn"
                and "delayed" not in wlow
                and not re.search(r"\b(?:wave|reinforcement|spawn|ambush)\s*#?\s*(?:[2-9]|\d{2,})\b", wlow)
                and not re.search(
                    r"\b(?:2nd|3rd|4th|5th|6th|7th|8th|second|third|fourth|fifth|sixth|seventh|eighth|"
                    r"reinforcements?|optional|bonus|escalation|dreadnought)\b",
                    wlow,
                )
            ):
                w["timing"] = "initial_spawn"
                _refresh_wave_link(w, "first_combat_wave_initial")
                promoted_anchor = True
            if promoted_anchor:
                pphase = w.get("parent_phase")
                if pphase:
                    promoted_sib = False
                    for sib in waves[w["wave_index"]:]:
                        if sib["timing"] == "static_structures":
                            continue
                        if sib.get("parent_phase") != pphase or sib.get("wave_trigger") is not None:
                            break
                        if sib["timing"] == "sequential_wave":
                            sib["timing"] = "simultaneous_initial_group"
                            _refresh_wave_link(sib, "table_separator_initial_group")
                            promoted_sib = True
                        elif sib["timing"] == "simultaneous_initial_group":
                            promoted_sib = True
                    if promoted_sib and not was_initial and w.get("group_name"):
                        w["timing"] = "simultaneous_initial_group"
                        _refresh_wave_link(w, "table_separator_initial_group")
            break

        # Step 2: Populate classified header wave_trigger targets and back-resolve source ships
        for w in waves:
            wt = w.get("wave_trigger")
            if not wt or wt.get("classification_state") == "unclassified":
                continue
            wt["target_wave_index"] = w["wave_index"]
            orig_low = (wt.get("original_text") or "").lower()
            for prev_w in waves[: w["wave_index"] - 1]:
                for s_idx, sp in enumerate(prev_w["spawns"], start=1):
                    rname = (sp.get("raw_name") or "").lower()
                    if len(rname) >= 5 and rname in orig_low:
                        wt["source_wave_index"] = prev_w["wave_index"]
                        wt["source_spawn_index"] = s_idx
                        wt.update(_identify_trigger_ship(sp, wt.get("original_text", ""), r_idx, prev_w["wave_index"], s_idx))
                        break
                if "source_wave_index" in wt:
                    break
            _refresh_wave_link(w)

        def _maybe_link_wave(tgt_w: dict[str, Any], payload: dict[str, Any]) -> None:
            cur_wt = tgt_w.get("wave_trigger")
            if (
                (cur_wt is None or cur_wt.get("classification_state") == "unclassified")
                and tgt_w["timing"] not in (*_INITIAL_WAVE_TIMINGS, "static_structures")
            ):
                tgt_w["wave_trigger"] = dict(payload, target_wave_index=tgt_w["wave_index"])
                _refresh_wave_link(tgt_w)

        def _wire_target_waves(
            tr: dict[str, Any],
            w_idx: int,
            s_idx: int,
            ship_meta: dict[str, Any],
            target_pairs: list[tuple[int, str | None]],
        ) -> None:
            target_indices = [idx for idx, _ in target_pairs]
            tr["target_wave_index"] = target_indices[0]
            if len(target_indices) > 1:
                tr["target_wave_indices"] = target_indices
            for t_idx, rule in target_pairs:
                st = "inferred" if rule else "explicit"
                trig_payload = {
                    "kind": tr["kind"],
                    "classification_state": st,
                    "inference_rule": rule,
                    "original_text": tr["original_text"],
                    "condition": tr["condition"],
                    "source_wave_index": w_idx,
                    "source_spawn_index": s_idx,
                    **ship_meta,
                }
                if tr.get("time_seconds") is not None:
                    trig_payload["time_seconds"] = tr["time_seconds"]
                _maybe_link_wave(waves[t_idx - 1], trig_payload)

        # Step 3: Wire classified spawn triggers forward; never wire unclassified triggers
        deferred_next: list[tuple[dict[str, Any], int, dict[str, Any], dict[str, Any]]] = []
        for w in waves:
            w_idx = w["wave_index"]
            for s_idx, sp in enumerate(w["spawns"], start=1):
                tr = sp.get("trigger")
                if not tr or tr.get("classification_state") == "unclassified":
                    continue
                ship_meta = _identify_trigger_ship(sp, tr.get("original_text", ""), r_idx, w_idx, s_idx)
                tr.update(ship_meta)
                explicit_pairs = _match_wave_targets_with_provenance(tr["original_text"], w_idx, waves)
                if explicit_pairs:
                    expanded: list[tuple[int, str | None]] = []
                    seen_exp: set[int] = set()
                    for t_idx, rule in explicit_pairs:
                        if t_idx not in seen_exp:
                            seen_exp.add(t_idx)
                            expanded.append((t_idx, rule))
                        pphase = waves[t_idx - 1].get("parent_phase")
                        if pphase:
                            for sib in waves[t_idx:]:
                                if sib["timing"] == "static_structures":
                                    continue
                                if sib.get("parent_phase") == pphase and sib["timing"] == "sequential_wave":
                                    if sib["wave_index"] not in seen_exp:
                                        seen_exp.add(sib["wave_index"])
                                        expanded.append((sib["wave_index"], rule))
                                else:
                                    break
                    _wire_target_waves(tr, w_idx, s_idx, ship_meta, expanded)
                elif tr["kind"] == "objective_complete":
                    continue
                elif not tr.get("is_escalation"):
                    orig_low = (tr.get("original_text") or "").lower()
                    has_explicit_num_ref = bool(
                        _KW_NUM_BEFORE_WAVE_RE.search(orig_low)
                        or _ORDINAL_BEFORE_WAVE_RE.search(orig_low)
                        or any(m.group(3) for m in _NAMED_WAVE_RE.finditer(orig_low))
                    )
                    is_explicit_spawn_cue = bool(
                        re.search(r"\b(?:next\s+(?:wave|spawn|group)|reinforcements?|triggers?|spawns?)\b", orig_low)
                    )
                    if is_explicit_spawn_cue and not has_explicit_num_ref:
                        deferred_next.append((w, s_idx, sp, ship_meta))

        for w, s_idx, sp, ship_meta in deferred_next:
            w_idx = w["wave_index"]
            tr = sp["trigger"]
            later_combat = [cw for cw in waves if _is_targetable_forward_wave(cw, w)]
            if not later_combat:
                continue
            tgt_w = later_combat[0]
            targets_for_next: list[tuple[int, str | None]] = [
                (tgt_w["wave_index"], "immediate_next_combat_wave")
            ]
            pphase = tgt_w.get("parent_phase")
            if pphase:
                for sib in later_combat[1:]:
                    if sib.get("parent_phase") == pphase and sib["timing"] == "sequential_wave":
                        targets_for_next.append((sib["wave_index"], "immediate_next_combat_wave"))
                    else:
                        break
            _wire_target_waves(tr, w_idx, s_idx, ship_meta, targets_for_next)


def _classify_wave_timing(
    wname: str,
    is_separator: bool,
    head_is_initial: bool,
    allow_static: bool = True,
    head_is_static: bool = False,
    has_header_trigger: bool = False,
) -> str:
    """Classify a wave's timing structurally from its header/group label."""
    low = wname.lower()
    if re.search(r"\b(?:possible|rare|optional|escalat|chance|random)\b", low):
        return "optional_spawn"
    if allow_static:
        if is_separator and head_is_static and not has_header_trigger and not _is_sequential_separator_label(wname):
            return "static_structures"
        struct_probe = _STATIC_STRUCT_SPATIAL_RE.sub(" ", low)
        if _STATIC_STRUCT_NOUN_RE.search(struct_probe) and not has_header_trigger and not re.search(
            r"\b(?:waves?|reinforcements?|spawns?)\b", low
        ):
            return "static_structures"
    if is_separator and head_is_initial and not has_header_trigger and not _is_sequential_separator_label(wname):
        return "simultaneous_initial_group"
    if not is_separator and not has_header_trigger and _INITIAL_WAVE_SPAWN_RE.search(low):
        return "initial_spawn"
    return "sequential_wave"


def _maybe_link_head_parent(waves: list[dict[str, Any]], end_idx: int, head_label: str) -> None:
    """Attach head_label as parent_phase onto its immediately preceding non-static head wave."""
    if not head_label:
        return
    for prev_idx in range(end_idx - 1, -1, -1):
        prev_w = waves[prev_idx]
        if prev_w["timing"] == "static_structures":
            continue
        if (
            prev_w["spawns"]
            and prev_w.get("parent_phase") is None
            and prev_w["wave_name"] == head_label
        ):
            prev_w["parent_phase"] = head_label
        break


def _build_wave_link(
    label: str,
    timing: str,
    wtrig: dict[str, Any] | None,
    is_separator: bool,
) -> dict[str, Any]:
    """Build 3-state wave_link provenance (explicit | inferred | unclassified) without G1 heuristics."""
    if wtrig is not None and (wtrig.get("classification_state") != "unclassified" or timing == "sequential_wave"):
        state = wtrig.get("classification_state", "unclassified")
        out: dict[str, Any] = {
            "classification_state": state,
            "inference_rule": wtrig.get("inference_rule"),
            "original_text": wtrig.get("from_wave_header") or wtrig.get("original_text") or label,
            "timing": timing,
            "trigger_kind": wtrig.get("kind"),
        }
        for opt_k in ("source_wave_index", "target_wave_index", "target_wave_indices", "trigger_ship_tag"):
            if wtrig.get(opt_k) is not None:
                out[opt_k] = wtrig[opt_k]
        return out
    if timing == "initial_spawn":
        rule = "table_implicit_initial" if label == "Initial" else "table_head_initial"
        return {
            "classification_state": "inferred",
            "inference_rule": rule,
            "original_text": label,
            "timing": timing,
            "trigger_kind": None,
        }
    if timing == "simultaneous_initial_group":
        return {
            "classification_state": "inferred",
            "inference_rule": "table_separator_initial_group",
            "original_text": label,
            "timing": timing,
            "trigger_kind": None,
        }
    if timing == "static_structures":
        rule = "table_separator_static_structures" if is_separator else "table_head_static_structures"
        return {
            "classification_state": "inferred",
            "inference_rule": rule,
            "original_text": label,
            "timing": timing,
            "trigger_kind": None,
        }
    return {
        "classification_state": "unclassified",
        "inference_rule": None,
        "original_text": label,
        "timing": timing,
        "trigger_kind": None,
    }


def _finalize_unit(
    page_title: str,
    tinfo: dict[str, Any],
    categories: list[str],
    is_event: bool,
    is_past: bool,
    unit_index: int,
    uraw: dict[str, Any],
    sde: SdeIndex,
) -> dict[str, Any]:
    hdr = uraw["header"]
    kv = hdr["kv"] if hdr else {}
    pos = hdr["pos_args"] if hdr else []

    factions: list[str] = list(tinfo["factions"])
    levels: list[int] = list(tinfo["levels"])
    unit_names: list[str] = [tinfo["base_title"]]

    if hdr and hdr["name"] == "missiondetails":
        if kv.get("name"):
            nm_clean = clean_wiki_inline(kv["name"])
            if nm_clean and nm_clean not in unit_names:
                unit_names.insert(0, nm_clean)
        for fk in ("faction", "faction1", "faction2", "faction3", "faction4"):
            if kv.get(fk):
                for fac in infer_factions_from_text(clean_wiki_inline(kv[fk])):
                    if fac not in factions:
                        factions.append(fac)
        if kv.get("level"):
            for m_l in re.findall(r"\b([1-5])\b", clean_wiki_inline(kv["level"])):
                lvl = int(m_l)
                if lvl not in levels:
                    levels.append(lvl)
    elif hdr:
        nm_clean = clean_wiki_inline(kv.get("name") or (pos[0] if pos else ""))
        if nm_clean and nm_clean not in unit_names:
            unit_names.insert(0, nm_clean)
        for fk in ("faction", "faction1", "faction2"):
            if kv.get(fk):
                for fac in infer_factions_from_text(clean_wiki_inline(kv[fk])):
                    if fac not in factions:
                        factions.append(fac)
        if kv.get("rating"):
            m_r = re.search(r"(\d+)\s*/\s*10", kv["rating"])
            if m_r:
                lvl = int(m_r.group(1))
                if lvl not in levels:
                    levels.append(lvl)

    # Fallback faction inference from section heading, title, or NPC names on the unit
    for probe in (uraw["section_heading"], tinfo["base_title"]):
        if probe:
            for f in infer_factions_from_text(probe):
                if f not in factions:
                    factions.append(f)

    if not factions:
        fac_counter: Counter[str] = Counter()
        for ev in uraw["events"]:
            if ev["kind"] == "template" and ev["name"] in ("npctablerow", "npctr"):
                epos, ekv = ev["pos_args"], ev["kv"]
                rname = epos[2] if len(epos) > 2 else ekv.get("name", "")
                for exp_n in sde.expand_short_names(rname)[0]:
                    f = infer_faction_from_text(exp_n)
                    if f:
                        fac_counter[f] += 1
        if fac_counter:
            factions.append(fac_counter.most_common(1)[0][0])

    # Parse rooms and waves in document order
    rooms: list[dict[str, Any]] = []
    cur_room: dict[str, Any] | None = None
    cur_wave: dict[str, Any] | None = None
    pending_room_name: str | None = None
    cur_head_label: str = ""
    cur_head_is_initial: bool = False
    cur_head_is_static: bool = False

    def ensure_room(name: str | None = None) -> dict[str, Any]:
        nonlocal cur_room, cur_wave, pending_room_name
        if cur_room is None:
            rname = name or pending_room_name or "Room 1"
            pending_room_name = None
            cur_room = {
                "room_index": len(rooms) + 1,
                "room_name": rname,
                "has_acceleration_gate": False,
                "waves": [],
            }
            rooms.append(cur_room)
            cur_wave = None
        return cur_room

    def start_room(name: str) -> None:
        nonlocal cur_room, cur_wave, pending_room_name, cur_head_label, cur_head_is_initial, cur_head_is_static, pending_heading_wave
        cur_head_label = ""
        cur_head_is_initial = False
        cur_head_is_static = False
        pending_heading_wave = None
        if cur_room is not None and not cur_room["waves"]:
            cur_room["room_name"] = name
            return
        cur_room = {
            "room_index": len(rooms) + 1,
            "room_name": name,
            "has_acceleration_gate": False,
            "waves": [],
        }
        rooms.append(cur_room)
        cur_wave = None
        pending_room_name = None

    pending_heading_wave: str | None = None

    def start_wave(
        wname: str,
        is_separator: bool = False,
        parent_phase: str | None = None,
        group_name: str | None = None,
        from_heading: bool = False,
        header_trigger: dict[str, Any] | None = None,
    ) -> None:
        nonlocal cur_wave, pending_heading_wave, cur_head_label, cur_head_is_initial, cur_head_is_static
        if from_heading:
            cur_head_label = ""
            cur_head_is_initial = False
            cur_head_is_static = False
        room = ensure_room()
        # Prune any immediately preceding empty wave (e.g. NPCTableHead|Initial Hostiles followed by NPCTableSeparator|Group 1)
        if room["waves"] and not room["waves"][-1]["spawns"]:
            room["waves"].pop()
        else:
            pending_heading_wave = None
        if pending_heading_wave and not is_separator and not from_heading:
            if not wname:
                wname = pending_heading_wave
            elif (
                not re.search(re.escape(pending_heading_wave), wname, re.I)
                and not re.search(r"\bwave\s*#?\s*\d+\b", wname, re.I)
            ):
                wname = f"{pending_heading_wave} - {wname}"
            pending_heading_wave = None
        elif not from_heading:
            pending_heading_wave = None
        label = wname or f"Wave {len(room['waves']) + 1}"
        if is_separator:
            wtrig = dict(header_trigger) if header_trigger is not None else None
        else:
            wtrig = classify_wave_header_trigger(label)
            if wtrig is not None and wtrig.get("classification_state") != "unclassified":
                parent_phase = None
        has_classified_wtrig = bool(wtrig is not None and wtrig.get("classification_state") != "unclassified")
        timing = _classify_wave_timing(
            group_name or label,
            is_separator=is_separator,
            head_is_initial=cur_head_is_initial,
            head_is_static=cur_head_is_static,
            has_header_trigger=has_classified_wtrig,
        )
        if not has_classified_wtrig and timing in (
            "initial_spawn",
            "static_structures",
            "simultaneous_initial_group",
        ):
            wtrig = None
        dist_info = extract_wave_distance_info(label, group_name=group_name)
        wave_link = _build_wave_link(label, timing, wtrig, is_separator=is_separator)
        wave_obj: dict[str, Any] = {
            "wave_index": len(room["waves"]) + 1,
            "wave_name": label,
            "timing": timing,
            "wave_trigger": wtrig,
            "wave_link": wave_link,
            "distance_m": dist_info["distance_m"],
            "spawns": [],
        }
        if dist_info["distance_unparsed"]:
            wave_obj["distance_raw"] = dist_info["distance_raw"]
            wave_obj["distance_unparsed"] = True
        if parent_phase:
            wave_obj["parent_phase"] = parent_phase
        if group_name:
            wave_obj["group_name"] = group_name
        cur_wave = wave_obj
        room["waves"].append(cur_wave)

    for ev in uraw["events"]:
        if ev["kind"] == "heading":
            htxt = ev["text"]
            m_rw = re.match(r"^(?:room|pocket)\s*(\d+)\b[,:\s-]+(.*\b(?:wave|group|spawn)\b.*)$", htxt, re.I)
            if m_rw:
                rnum = m_rw.group(1)
                if cur_room is None:
                    pending_room_name = f"Room {rnum}"
                elif not re.search(rf"\b(?:room|pocket)\s*{re.escape(rnum)}\b", cur_room["room_name"], re.I):
                    start_room(f"Room {rnum}")
                h_wave = m_rw.group(2).strip(" ,:-")
                start_wave(htxt, is_separator=False, from_heading=True)
                pending_heading_wave = h_wave or htxt
                continue
            if re.match(r"^(?:wave|reinforcements?|spawn)\s*\d+\b", htxt, re.I):
                start_wave(htxt, is_separator=False, from_heading=True)
                pending_heading_wave = htxt
                continue
            if ROOM_HEADING_RE.search(htxt) and not re.search(r"\bescalation\b", htxt, re.I):
                if cur_room is None:
                    pending_room_name = htxt
                else:
                    start_room(htxt)
            elif cur_room is None and re.search(r"\b(?:pocket|room|area|level)\b", htxt, re.I):
                pending_room_name = htxt
            continue

        tname = ev["name"]
        epos = ev["pos_args"]
        ekv = ev["kv"]
        if tname in ("npctablehead", "npcth"):
            wname = clean_wiki_inline(epos[0] if epos else ekv.get("wave", ""))
            if wname and re.match(r"^(?:pocket|room)\s+\d+\b", wname, re.I) and cur_room is not None and cur_room["waves"]:
                start_room(wname)
            start_wave(wname, is_separator=False)
            cur_head_label = cur_wave["wave_name"] if cur_wave is not None else wname
            cur_head_wtrig = cur_wave.get("wave_trigger") if cur_wave is not None else None
            cur_head_is_initial = bool(
                _INITIAL_HEAD_RE.search(cur_head_label)
                and not (cur_head_wtrig is not None and cur_head_wtrig.get("classification_state") != "unclassified")
            )
            cur_head_is_static = bool(cur_wave is not None and cur_wave["timing"] == "static_structures")
        elif tname in ("npctableseparator", "npcts"):
            sep_label = clean_wiki_inline(epos[0] if epos else "")
            if sep_label and re.match(r"^(?:pocket|room)\s+\d+\b", sep_label, re.I):
                start_room(sep_label)
            else:
                grp = sep_label or "Group"
                full_label = (
                    f"{cur_head_label} - {grp}"
                    if cur_head_label and grp.lower() != cur_head_label.lower()
                    else grp
                )
                sep_own_wtrig = classify_wave_header_trigger(grp)
                head_wtrig = classify_wave_header_trigger(cur_head_label) if cur_head_label else None
                sep_own_classified = bool(
                    sep_own_wtrig is not None and sep_own_wtrig.get("classification_state") != "unclassified"
                )
                head_classified = bool(
                    head_wtrig is not None and head_wtrig.get("classification_state") != "unclassified"
                )
                is_sequential_sep = sep_own_classified or _is_sequential_separator_label(grp)
                # Sequential separators (e.g. Wave 2 under a triggered Wave 1 head) never inherit parent head's trigger
                fallback_wtrig = None if is_sequential_sep else (
                    dict(head_wtrig) if head_classified else (sep_own_wtrig or (dict(head_wtrig) if head_wtrig else None))
                )
                has_sep_trig = sep_own_classified or (head_classified and not is_sequential_sep)
                sep_timing = _classify_wave_timing(
                    grp,
                    is_separator=True,
                    head_is_initial=False if is_sequential_sep else cur_head_is_initial,
                    head_is_static=False if is_sequential_sep else cur_head_is_static,
                    has_header_trigger=has_sep_trig,
                )
                sep_wtrig = (
                    sep_own_wtrig
                    if (sep_own_classified or (is_sequential_sep and sep_own_wtrig is not None))
                    else (None if sep_timing in ("static_structures", "simultaneous_initial_group") else fallback_wtrig)
                )
                sep_parent_phase: str | None = None if is_sequential_sep else (cur_head_label or None)
                if (
                    not is_sequential_sep
                    and sep_timing != "static_structures"
                    and cur_head_label
                    and cur_room is not None
                    and cur_room["waves"]
                ):
                    _maybe_link_head_parent(cur_room["waves"], len(cur_room["waves"]), cur_head_label)
                start_wave(
                    full_label,
                    is_separator=True,
                    parent_phase=sep_parent_phase,
                    group_name=grp,
                    header_trigger=sep_wtrig,
                )
                if is_sequential_sep:
                    cur_head_label = (
                        grp
                        if (not sep_own_classified or re.match(r"^(?:initial\b|wave\b|stage\b|phase\b)", grp, re.I))
                        else ""
                    )
                    cur_head_is_initial = False
                    cur_head_is_static = bool(cur_head_label and cur_wave is not None and cur_wave["timing"] == "static_structures")
        elif tname in ("npctablerow", "npctr"):
            if cur_wave is None:
                start_wave("Initial")
            assert cur_room is not None and cur_wave is not None
            raw_cls = clean_wiki_inline(epos[0] if len(epos) > 0 else ekv.get("class", ""))
            raw_cnt = epos[1] if len(epos) > 1 else ekv.get("count", "")
            raw_nm = epos[2] if len(epos) > 2 else ekv.get("name", "")
            raw_trig = ekv.get("trigger", "")
            raw_point = ekv.get("point") or ekv.get("piont") or ekv.get("opint") or ""
            raw_note = clean_wiki_inline(ekv.get("note") or ekv.get("node") or ekv.get("noye") or "")
            raw_drop = clean_wiki_inline(ekv.get("drop", ""))
            raw_cargo = clean_wiki_inline(ekv.get("cargo", ""))
            raw_loot = clean_wiki_inline(ekv.get("loot", ""))
            raw_ewars = [
                ekv.get(k, "")
                for k in ("ewar", "ewar2", "ewar3", "ewawr", "ewarp", "damp")
                if ekv.get(k)
            ]

            point_tag = parse_point(raw_point)
            ewar_info = parse_ewar(raw_ewars, point_tag)
            count_info = parse_count(raw_cnt)
            trig_info = classify_trigger(raw_trig)
            res = sde.resolve_npc_row(
                npc_class=raw_cls,
                raw_name=raw_nm,
                note=raw_note,
                page_factions=factions,
                is_event=is_event,
                include_name_map=True,
            )

            if raw_cls.lower() in ("acceleration gate", "accl gate", "accel gate") or re.match(
                r"^(?:acceleration )?gate\b", clean_wiki_inline(raw_nm).lower()
            ):
                cur_room["has_acceleration_gate"] = True

            spawn_entry: dict[str, Any] = {
                "raw_class": raw_cls,
                "raw_name": clean_wiki_inline(raw_nm),
                "count": count_info,
                "resolution": res,
                "trigger": trig_info,
                "ewar": ewar_info,
                "point": point_tag,
            }
            if raw_drop:
                spawn_entry["drop"] = raw_drop
            if raw_cargo:
                spawn_entry["cargo"] = raw_cargo
            if raw_loot:
                spawn_entry["loot"] = raw_loot
            if raw_note:
                spawn_entry["note"] = raw_note
            if ekv.get("destructible"):
                spawn_entry["destructible"] = clean_wiki_inline(ekv["destructible"])

            cur_wave["spawns"].append(spawn_entry)

    # Drop empty ghost waves inside each room and empty rooms with no spawns
    for r in rooms:
        r["waves"] = [w for w in r["waves"] if w["spawns"]]
        for w_idx, w in enumerate(r["waves"], start=1):
            w["wave_index"] = w_idx
            # A wave containing actual combat warships (unless passive/neutral) is never static_structures
            if (
                w["timing"] == "static_structures"
                and not re.search(r"\b(?:passive|neutral|friendly|civilian)\b", w["wave_name"], re.I)
                and any(_COMBAT_SHIP_CLASS_RE.search(sp["raw_class"]) for sp in w["spawns"])
            ):
                pphase = w.get("parent_phase") or ""
                own_label = w.get("group_name") or w["wave_name"]
                own_wtrig = classify_wave_header_trigger(own_label)
                own_has_trig = bool(
                    own_wtrig is not None and own_wtrig.get("classification_state") != "unclassified"
                )
                pphase_wtrig = classify_wave_header_trigger(pphase) if pphase else None
                pphase_has_trig = bool(
                    pphase_wtrig is not None and pphase_wtrig.get("classification_state") != "unclassified"
                )
                is_sep = bool(w.get("group_name"))
                has_trig = own_has_trig or pphase_has_trig or bool(
                    w.get("wave_trigger") is not None
                    and w["wave_trigger"].get("classification_state") != "unclassified"
                )
                w["timing"] = _classify_wave_timing(
                    own_label,
                    is_separator=is_sep,
                    head_is_initial=bool(pphase and _INITIAL_HEAD_RE.search(pphase) and not pphase_has_trig),
                    allow_static=False,
                    has_header_trigger=has_trig,
                )
                if w.get("wave_trigger") is None and w["timing"] not in (
                    "static_structures",
                    "initial_spawn",
                    "simultaneous_initial_group",
                ):
                    w["wave_trigger"] = (
                        dict(own_wtrig)
                        if own_has_trig
                        else (dict(pphase_wtrig) if pphase_has_trig else (own_wtrig or (dict(pphase_wtrig) if pphase_wtrig else None)))
                    )
                w["wave_link"] = _build_wave_link(w["wave_name"], w["timing"], w.get("wave_trigger"), is_separator=is_sep)
                if is_sep and pphase and w["timing"] != "static_structures":
                    _maybe_link_head_parent(r["waves"], w_idx - 1, pphase)
    rooms = [r for r in rooms if r["waves"]]
    for idx, r in enumerate(rooms, start=1):
        r["room_index"] = idx
    wire_room_waves_and_triggers(rooms)

    # Determine part_info from title qualifier OR section/stage heading (e.g. 'Part 2 of 3', 'Second Location')
    unit_part_info = uraw.get("stage_part_info") or tinfo["part_info"]
    if unit_part_info is None and uraw.get("section_heading"):
        sh_hit = _parse_stage_heading(uraw["section_heading"])
        if sh_hit and sh_hit[1]:
            unit_part_info = {"part": sh_hit[0], "of": sh_hit[1]}

    # Extract courier & mining payloads from Missiondetails
    mtype_raw = clean_wiki_inline(kv.get("type", "")) if hdr else ""
    obj_raw = kv.get("objective", "") or kv.get("objestive", "") if hdr else ""
    courier_payload = None
    mining_payload = None
    mission_hint: str | None = None
    if hdr and hdr["name"] == "missiondetails":
        mt_low = mtype_raw.lower()
        if "courier" in mt_low or "distribution" in mt_low or "trade" in mt_low:
            courier_payload = extract_courier_payload(obj_raw, sde)
            if "courier" in mt_low or "distribution" in mt_low:
                mission_hint = "courier"
        if "mining" in mt_low or re.search(r"\b(?:mine|harvest)\b", clean_wiki_inline(obj_raw).lower()):
            cand_mining = extract_mining_payload(obj_raw, sde)
            if cand_mining is not None and ("mining" in mt_low or bool(cand_mining.get("ore_type_ids"))):
                mining_payload = cand_mining
        if mission_hint is None and (rooms or "encounter" in mt_low or "security" in mt_low):
            mission_hint = "non_courier"

    # Build SDE matching candidate names (prefer explicit part-qualified titles before base fallback)
    sde_match: dict[str, Any] | None = None
    base_clean = unit_names[0]
    part_base = re.sub(r"\s*\(\d+\s+of\s+\d+\)\s*$", "", base_clean).strip() or base_clean
    part_base_low = norm_title(part_base).lower()
    stage_ref = uraw.get("stage_heading") or uraw.get("section_heading")
    if unit_part_info and (stage_ref or re.search(r"\(\d+\s+of\s+\d+\)", base_clean)):
        p_num = unit_part_info["part"]
        p_of = unit_part_info.get("of")
        part_candidates = [
            f"{part_base}. part {p_num}.",
            f"{part_base} part {p_num}",
        ]
        if p_of:
            part_candidates.insert(0, f"{part_base} ({p_num} of {p_of})")
            part_candidates.append(f"{part_base} ({p_num} of {p_of}")
        if (
            stage_ref
            and not STAGE_HEADING_RE.match(stage_ref)
            and part_base_low in norm_title(stage_ref).lower()
            and re.search(r"\(\d+\s+of\s+\d+\)|\bpart\s*#?\d+\b", stage_ref, re.I)
            and stage_ref not in part_candidates
        ):
            part_candidates.append(stage_ref)
        part_match = sde.match_sde_page(
            part_candidates, factions, levels, part_info=unit_part_info, mission_hint=mission_hint
        )
        if part_match["mission_ids"] or part_match["dungeon_ids"]:
            sde_match = part_match

    if sde_match is None:
        sde_match = sde.match_sde_page(
            list(unit_names), factions, levels, part_info=unit_part_info, mission_hint=mission_hint
        )
        if not sde_match["mission_ids"] and not sde_match["dungeon_ids"] and uraw["section_heading"]:
            sh = uraw["section_heading"]
            sh_parts = [seg.strip() for seg in sh.split(" > ") if seg.strip()] if " > " in sh else [sh]
            for sh_cand in reversed(sh_parts):
                if not STAGE_HEADING_RE.match(sh_cand) and re.search(r"\(\d+\s+of\s+\d+\)", sh_cand):
                    sde_match = sde.match_sde_page(
                        [sh_cand], factions, levels, part_info=unit_part_info, mission_hint=mission_hint
                    )
                    if sde_match["mission_ids"] or sde_match["dungeon_ids"]:
                        break

    meta: dict[str, Any] = {}
    if hdr and hdr["name"] == "missiondetails":
        meta = {
            "mission_type": mtype_raw,
            "objective": clean_wiki_inline(obj_raw),
            "ship_size_limit": clean_wiki_inline(kv.get("shipsizelimit") or kv.get("shipssizelimit") or ""),
            "ship_suggestion": clean_wiki_inline(kv.get("shipsuggestion", "")),
            "rewards": clean_wiki_inline(kv.get("rewards", "")),
            "extra": clean_wiki_inline(kv.get("extra") or kv.get("extras") or ""),
            "standing_loss": clean_wiki_inline(kv.get("standingloss") or kv.get("standing loss") or ""),
            "storyline": clean_wiki_inline(kv.get("storyline", "")),
            "damage_to_deal": clean_wiki_inline(kv.get("damagetodeal") or kv.get("damagetodeal1") or ""),
            "damage_to_resist": clean_wiki_inline(kv.get("damagetoresist") or kv.get("damagetoresist1") or ""),
        }
    elif hdr:
        meta = {
            "site_type": clean_wiki_inline(kv.get("type", "")),
            "location": clean_wiki_inline(kv.get("location", "")),
            "rating": clean_wiki_inline(kv.get("rating", "")),
            "ship_limit": clean_wiki_inline(kv.get("ship limit", "")),
            "signature_strength": clean_wiki_inline(kv.get("signature strength", "")),
            "damage": clean_wiki_inline(kv.get("damage", "")),
            "resist": clean_wiki_inline(kv.get("resist", "")),
        }

    structured_obj = build_structured_objective(
        meta=meta,
        courier_payload=courier_payload,
        mining_payload=mining_payload,
        rooms=rooms,
        sde_match=sde_match,
        sde=sde,
        categories=categories,
    )

    entry_id = f"{page_title}#{unit_index}"
    unit_out: dict[str, Any] = {
        "entry_id": entry_id,
        "page_title": page_title,
        "base_title": tinfo["base_title"],
        "unit_index": unit_index,
        "unit_kind": uraw["unit_kind"],
        "section_heading": uraw["section_heading"],
        "variant_name": uraw.get("variant_name"),
        "title_qualifiers": tinfo["qualifiers"],
        "part_info": unit_part_info,
        "categories": sorted(categories),
        "is_event": is_event,
        "is_past_content": is_past,
        "factions": factions,
        "levels": levels,
        "objective_kind": structured_obj["kind"],
        "objective": structured_obj,
        "metadata": meta,
        "courier": courier_payload,
        "mining": mining_payload,
        "escalation_links": sorted(set(uraw["escalation_links"])),
        "sde_matches": {
            "mission_ids": sde_match["mission_ids"],
            "dungeon_ids": sde_match["dungeon_ids"],
        },
        "rooms": rooms,
    }
    if "variant_index" in uraw:
        unit_out["variant_index"] = uraw["variant_index"]
        unit_out["variant_count"] = uraw["variant_count"]
    return unit_out


def _collect_wiki_articles(wiki_dir: Path) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Load article list deterministically from catalog.json or fallback directory scan."""
    catalog_path = wiki_dir / "catalog.json"
    articles: list[dict[str, Any]] = []
    catalog_summary: dict[str, Any] = {}
    if catalog_path.exists():
        cat = json.loads(catalog_path.read_text(encoding="utf-8"))
        catalog_summary = cat.get("summary", {})
        for slug in sorted(cat.get("articles", {})):
            meta = cat["articles"][slug]
            title = meta.get("title") or slug.replace("_", " ")
            if title.startswith(SKIP_TEMPLATE_TITLES_PREFIX):
                continue
            rel = meta.get("wikitext_path") or f"wikitext/{slug}.wiki"
            wpath = wiki_dir / rel
            if not wpath.exists():
                continue
            articles.append(
                {
                    "slug": slug,
                    "title": title,
                    "categories": list(meta.get("categories", [])),
                    "path": wpath,
                }
            )
    else:
        wdir = wiki_dir / "wikitext"
        search_dir = wdir if wdir.exists() else wiki_dir
        for wpath in sorted(search_dir.glob("*.wiki")):
            title = wpath.stem.replace("_", " ")
            if title.startswith(SKIP_TEMPLATE_TITLES_PREFIX):
                continue
            articles.append(
                {
                    "slug": wpath.stem,
                    "title": title,
                    "categories": [],
                    "path": wpath,
                }
            )
    articles.sort(key=lambda a: (a["title"], a["slug"]))
    return articles, catalog_summary


def _wire_escalation_chains(all_units: list[dict[str, Any]]) -> None:
    """Resolve same-page expedition stages, (k of N) mission series, and cross-page escalation links."""
    by_page_low: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_base_low: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_slug_low: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_entry_id: dict[str, dict[str, Any]] = {}

    def _norm_lk(name: str) -> str:
        cleaned = name.split("#", 1)[0].replace("_", " ")
        return re.sub(r"[^\w\s-]", "", norm_title(cleaned)).strip().lower()

    for u in all_units:
        by_entry_id[u["entry_id"]] = u
        by_page_low[norm_title(u["page_title"]).lower()].append(u)
        by_base_low[norm_title(u["base_title"]).lower()].append(u)
        by_slug_low[_norm_lk(u["page_title"])].append(u)
        by_slug_low[_norm_lk(u["base_title"])].append(u)

    def _append_link(u: dict[str, Any], link: str) -> None:
        if link and link not in u["escalation_links"]:
            u["escalation_links"].append(link)

    # 1. Multi-stage pages (e.g. Location 1..4, Part 1..4, Missiondetails Part 1..3 on same page)
    stage_groups: dict[tuple[str, tuple[str, ...], tuple[int, ...]], list[dict[str, Any]]] = defaultdict(list)
    for u in all_units:
        pinfo = u.get("part_info")
        if pinfo:
            stage_groups[(u["page_title"], tuple(sorted(u["factions"])), tuple(u["levels"]))].append(u)
    for group in stage_groups.values():
        if len(group) > 1:
            by_part: dict[int, list[dict[str, Any]]] = defaultdict(list)
            for u in group:
                by_part[int(u["part_info"]["part"])].append(u)
            sorted_parts = sorted(by_part)
            for p_idx in range(len(sorted_parts) - 1):
                cur_list = by_part[sorted_parts[p_idx]]
                nxt_list = sorted(by_part[sorted_parts[p_idx + 1]], key=lambda x: (x["unit_index"], x["entry_id"]))
                chosen_nxt = nxt_list[0]["entry_id"]
                for cur_u in cur_list:
                    if chosen_nxt != cur_u["entry_id"]:
                        cur_u["next_stage_entry_id"] = chosen_nxt
                        _append_link(cur_u, chosen_nxt)

    # 2. Multi-part (k of N) mission series across separate pages
    series_groups: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for u in all_units:
        pinfo = u.get("part_info")
        if not pinfo or not pinfo.get("of") or int(pinfo["of"]) <= 1:
            continue
        series_nm = re.sub(r"\s*\(\d+\s+of\s+\d+\)\s*$", "", norm_title(u["base_title"])).strip().lower()
        if not series_nm:
            continue
        series_groups[series_nm].append(u)
    for group in series_groups.values():
        pages = {u["page_title"] for u in group}
        if len(pages) < 2:
            continue
        by_part: dict[int, list[dict[str, Any]]] = defaultdict(list)
        for u in group:
            by_part[int(u["part_info"]["part"])].append(u)
        for p_num, cur_list in by_part.items():
            nxt_list = by_part.get(p_num + 1)
            if not nxt_list:
                continue
            for u in cur_list:
                non_self = [nu for nu in nxt_list if nu["entry_id"] != u["entry_id"]]
                u_facs = set(u["factions"])
                compat = [
                    nu for nu in non_self
                    if not u_facs or not nu["factions"] or (set(nu["factions"]) & u_facs)
                ]
                if not compat:
                    continue
                fac_match = [nu for nu in compat if u_facs and (set(nu["factions"]) & u_facs)]
                cand_base = fac_match or compat
                lvl_match = [nu for nu in cand_base if nu["levels"] == u["levels"]]
                pool = lvl_match or cand_base
                chosen = sorted(pool, key=lambda x: x["entry_id"])[0]
                if "next_stage_entry_id" not in u:
                    u["next_stage_entry_id"] = chosen["entry_id"]
                _append_link(u, chosen["entry_id"])

    # 3. Resolve cross-page prose/template escalation titles into concrete target entry_ids when present
    for u in all_units:
        resolved_links: list[str] = []
        target_ids: list[str] = []
        u_facs = set(u["factions"])
        for lk in u["escalation_links"]:
            if lk == u["entry_id"]:
                continue
            if lk in by_entry_id:
                cand_id = lk
                raw_cands = [by_entry_id[lk]]
            else:
                low = norm_title(lk.split("#", 1)[0].replace("_", " ")).lower()
                raw_cands = by_page_low.get(low) or by_base_low.get(low) or by_slug_low.get(_norm_lk(lk)) or []
                cands = [c for c in raw_cands if c["entry_id"] != u["entry_id"]]
                cand_id = ""
                if cands:
                    same_fac = [c for c in cands if u_facs and (set(c["factions"]) & u_facs)]
                    pool = same_fac or cands
                    part1 = [c for c in pool if (c.get("part_info") or {}).get("part") == 1]
                    chosen = sorted(part1 or pool, key=lambda x: (x["unit_index"], x["entry_id"]))[0]
                    cand_id = chosen["entry_id"]
            if cand_id:
                if cand_id not in resolved_links:
                    resolved_links.append(cand_id)
                if cand_id not in target_ids:
                    target_ids.append(cand_id)
            elif not raw_cands and lk not in resolved_links:
                resolved_links.append(lk)
        u["escalation_links"] = resolved_links
        if target_ids:
            u["escalation_target_ids"] = target_ids


def _build_disambiguated_indexes(matched_units: list[dict[str, Any]]) -> dict[str, Any]:
    """Build 1-to-1 primary indexes keyed by '{id}:{faction_id}:{level}:{variant}' with zero collisions."""
    def _priority(u: dict[str, Any]) -> tuple[int, int, int, int, str]:
        p_low = norm_title(u["page_title"]).lower()
        b_low = norm_title(u["base_title"]).lower()
        is_subpage = 0 if (p_low == b_low and "/" not in u["page_title"]) else 1
        is_ded = 1 if any(q.lower() == "ded" for q in u.get("title_qualifiers", [])) else 0
        spawn_cnt = -sum(len(w["spawns"]) for r in u["rooms"] for w in r["waves"])
        return (is_subpage, is_ded, spawn_cnt, u["unit_index"], u["entry_id"])

    by_mission_key: dict[str, str] = {}
    by_dungeon_key: dict[str, str] = {}
    by_mission_id: dict[str, set[str]] = defaultdict(set)
    by_dungeon_id: dict[str, set[str]] = defaultdict(set)

    def _base_variant(u: dict[str, Any]) -> str:
        parts: list[str] = []
        pinfo = u.get("part_info")
        if pinfo:
            parts.append(f"part_{pinfo['part']}_of_{pinfo['of']}" if pinfo.get("of") else f"part_{pinfo['part']}")
        if u.get("variant_name"):
            parts.append(str(u["variant_name"]).replace(":", "_").strip())
        return "__".join(parts)

    def _insert_primary(key_map: dict[str, str], rec_id: int, fid: int, lvl: int, base_var: str, u: dict[str, Any]) -> None:
        eid = u["entry_id"]
        cand_key = f"{rec_id}:{fid}:{lvl}:{base_var}"
        if cand_key not in key_map or key_map[cand_key] == eid:
            key_map[cand_key] = eid
            return
        # Disambiguate remaining identical-signature entries deterministically
        sec_slug = re.sub(r"[^A-Za-z0-9_-]+", "_", norm_title(u.get("section_heading") or u["page_title"])).strip("_")
        suffix = f"{sec_slug}_u{u['unit_index']}" if u["page_title"] != u["base_title"] or u.get("section_heading") else f"u{u['unit_index']}"
        var2 = f"{base_var}__{suffix}" if base_var else suffix
        cand2 = f"{rec_id}:{fid}:{lvl}:{var2}"
        idx_num = 2
        while cand2 in key_map and key_map[cand2] != eid:
            cand2 = f"{rec_id}:{fid}:{lvl}:{var2}_{idx_num}"
            idx_num += 1
        key_map[cand2] = eid

    for u in sorted(matched_units, key=_priority):
        eid = u["entry_id"]
        fids = sorted({CANON_FACTION_NAME_TO_ID[f] for f in u["factions"] if f in CANON_FACTION_NAME_TO_ID}) or [0]
        lvls = sorted(set(u["levels"])) or [0]
        base_var = _base_variant(u)
        for mid in u["sde_matches"]["mission_ids"]:
            by_mission_id[str(mid)].add(eid)
            for fid in fids:
                for lvl in lvls:
                    _insert_primary(by_mission_key, mid, fid, lvl, base_var, u)
        for did in u["sde_matches"]["dungeon_ids"]:
            by_dungeon_id[str(did)].add(eid)
            for fid in fids:
                for lvl in lvls:
                    _insert_primary(by_dungeon_key, did, fid, lvl, base_var, u)

    return {
        "by_mission_key": dict(sorted(by_mission_key.items())),
        "by_dungeon_key": dict(sorted(by_dungeon_key.items())),
        "by_mission_id": {k: sorted(v) for k, v in sorted(by_mission_id.items())},
        "by_dungeon_id": {k: sorted(v) for k, v in sorted(by_dungeon_id.items())},
    }


def _compact_trigger_for_runtime(tr: dict[str, Any] | None) -> dict[str, Any] | None:
    if tr is None:
        return None
    out: dict[str, Any] = {
        "kind": tr.get("kind"),
        "classification_state": tr.get("classification_state", "unclassified"),
        "inference_rule": tr.get("inference_rule"),
        "original_text": tr.get("original_text", ""),
        "condition": tr.get("condition"),
    }
    for opt_k in (
        "time_seconds",
        "unlocks_gate",
        "is_escalation",
        "source_wave_index",
        "source_spawn_index",
        "target_wave_index",
        "target_wave_indices",
        "trigger_ship_tag",
        "trigger_ship_name",
        "trigger_type_ids",
        "from_wave_header",
        "override_applied",
    ):
        if opt_k in tr and tr[opt_k] is not None:
            out[opt_k] = tr[opt_k]
    return out


def _compact_entry_for_runtime(u: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """Strip debug/prose bloat for compact schema_version=2 runtime JSON, returning (runtime_entry, prose_dump)."""
    prose_dump: dict[str, Any] = {}
    meta_full = u.get("metadata") or {}
    meta_compact: dict[str, Any] = {}
    prose_meta_keys = {"objective", "rewards", "extra", "standing_loss", "ship_suggestion"}
    for mk, mv in meta_full.items():
        if not mv:
            continue
        if mk in prose_meta_keys:
            prose_dump[mk] = mv
        else:
            meta_compact[mk] = mv

    rooms_compact: list[dict[str, Any]] = []
    for r in u["rooms"]:
        waves_compact: list[dict[str, Any]] = []
        for w in r["waves"]:
            spawns_compact: list[dict[str, Any]] = []
            for sp in w["spawns"]:
                sres = sp["resolution"]
                res_compact: dict[str, Any] = {
                    "status": sres["status"],
                    "type_ids": sres.get("type_ids", []),
                }
                if sres["status"] == "resolved_class_candidates" and sres.get("candidate_type_ids"):
                    res_compact["candidate_type_ids"] = sres["candidate_type_ids"]
                if sres.get("had_ellipsis"):
                    res_compact["had_ellipsis"] = True

                sc: dict[str, Any] = {
                    "raw_class": sp["raw_class"],
                    "raw_name": sp["raw_name"],
                    "count": {"min": sp["count"]["min"], "max": sp["count"]["max"]},
                    "resolution": res_compact,
                }
                if sp.get("trigger"):
                    sc["trigger"] = _compact_trigger_for_runtime(sp["trigger"])
                if sp.get("ewar") and sp["ewar"].get("tags"):
                    sc["ewar"] = {"tags": sp["ewar"]["tags"]}
                if sp.get("point"):
                    sc["point"] = sp["point"]
                for extra_k in ("drop", "cargo", "loot", "destructible"):
                    if sp.get(extra_k):
                        sc[extra_k] = sp[extra_k]
                spawns_compact.append(sc)

            wc: dict[str, Any] = {
                "wave_index": w["wave_index"],
                "wave_name": w["wave_name"],
                "timing": w["timing"],
                "wave_trigger": _compact_trigger_for_runtime(w.get("wave_trigger")),
                "wave_link": w["wave_link"],
                "spawns": spawns_compact,
            }
            if w.get("distance_m") is not None:
                wc["distance_m"] = w["distance_m"]
            elif w.get("distance_unparsed"):
                wc["distance_m"] = None
                wc["distance_raw"] = w["distance_raw"]
                wc["distance_unparsed"] = True
            waves_compact.append(wc)

        rooms_compact.append(
            {
                "room_index": r["room_index"],
                "room_name": r["room_name"],
                "has_acceleration_gate": r["has_acceleration_gate"],
                "waves": waves_compact,
            }
        )

    out: dict[str, Any] = {
        "entry_id": u["entry_id"],
        "page_title": u["page_title"],
        "base_title": u["base_title"],
        "unit_index": u["unit_index"],
        "unit_kind": u["unit_kind"],
        "factions": u["factions"],
        "levels": u["levels"],
        "objective_kind": u["objective_kind"],
        "objective": u["objective"],
        "escalation_links": u["escalation_links"],
        "sde_matches": u["sde_matches"],
        "rooms": rooms_compact,
    }
    if u.get("categories"):
        out["categories"] = u["categories"]
    if u.get("section_heading"):
        out["section_heading"] = u["section_heading"]
    if u.get("variant_name") is not None:
        out["variant_name"] = u["variant_name"]
    if "variant_index" in u:
        out["variant_index"] = u["variant_index"]
        out["variant_count"] = u["variant_count"]
    if u.get("part_info") is not None:
        out["part_info"] = u["part_info"]
    if u.get("is_event"):
        out["is_event"] = True
    if u.get("is_past_content"):
        out["is_past_content"] = True
    if meta_compact:
        out["metadata"] = meta_compact
    if u.get("courier"):
        out["courier"] = {k: v for k, v in u["courier"].items() if k != "raw"}
    if u.get("mining"):
        out["mining"] = {k: v for k, v in u["mining"].items() if k != "raw"}
    if u.get("escalation_target_ids"):
        out["escalation_target_ids"] = u["escalation_target_ids"]
    if u.get("next_stage_entry_id"):
        out["next_stage_entry_id"] = u["next_stage_entry_id"]

    if prose_dump:
        prose_dump["entry_id"] = u["entry_id"]
        return out, prose_dump
    return out, None


_ALLOWED_WAVE_TIMINGS = frozenset(
    {
        "initial_spawn",
        "simultaneous_initial_group",
        "sequential_wave",
        "static_structures",
        "optional_spawn",
    }
)
_ALLOWED_TRIGGER_CONDITIONS = frozenset({"on_destroy", "on_attack", "on_damage", "on_timer"})
_ALLOWED_OVERRIDE_STATES = ("explicit", "inferred")


def _validate_positive_int_key(key: Any, ctx: str) -> int:
    if not isinstance(key, str) or not key.isdigit() or int(key) <= 0:
        raise ValueError(f"Invalid positive integer index key {key!r} in {ctx}")
    return int(key)


def _validate_state_and_rule(spec: dict[str, Any], ctx: str) -> tuple[str, str | None]:
    state = spec.get("classification_state", "explicit")
    if state not in _ALLOWED_OVERRIDE_STATES:
        raise ValueError(f"Invalid classification_state {state!r} in {ctx}; expected {_ALLOWED_OVERRIDE_STATES}")
    rule = spec.get("inference_rule")
    if state == "inferred":
        if not isinstance(rule, str) or not rule.strip():
            raise ValueError(f"Inferred override in {ctx} requires non-empty inference_rule")
    elif rule is not None:
        raise ValueError(f"Explicit override in {ctx} must not set inference_rule ({rule!r})")
    return state, rule


def _validate_waves_override_map(waves_spec: Any, ctx: str, *, allow_room_index: bool = True) -> None:
    if not isinstance(waves_spec, dict):
        raise ValueError(f"Expected dict for waves in {ctx}")
    allowed_wave_keys = {
        "timing", "trigger_kind", "condition", "time_seconds",
        "source_wave_index", "source_spawn_index", "target_wave_index", "target_wave_indices",
        "classification_state", "inference_rule", "spawns", "notes",
    }
    if allow_room_index:
        allowed_wave_keys.add("room_index")
    allowed_spawn_keys = {
        "kind", "condition", "time_seconds", "target_wave_index", "target_wave_indices",
        "classification_state", "inference_rule", "notes",
    }
    for w_key, w_spec in waves_spec.items():
        w_idx = _validate_positive_int_key(w_key, ctx)
        wctx = f"{ctx}.waves[{w_idx}]"
        if not isinstance(w_spec, dict):
            raise ValueError(f"Expected dict in {wctx}")
        bad_keys = set(w_spec.keys()) - allowed_wave_keys
        if bad_keys:
            raise ValueError(f"Unknown wave override keys {sorted(bad_keys)} in {wctx}")
        if "room_index" in w_spec:
            ri = w_spec["room_index"]
            if not isinstance(ri, int) or isinstance(ri, bool) or ri < 1:
                raise ValueError(f"Invalid room_index {ri!r} in {wctx}")
        timing = w_spec.get("timing")
        if timing is not None and timing not in _ALLOWED_WAVE_TIMINGS:
            raise ValueError(f"Invalid timing {timing!r} in {wctx}")
        if "trigger_kind" in w_spec:
            tk = w_spec["trigger_kind"]
            if tk is not None and tk not in TRIGGER_ENUM:
                raise ValueError(f"Invalid trigger_kind {tk!r} in {wctx}")
            if tk is not None and timing in (*_INITIAL_WAVE_TIMINGS, "static_structures"):
                raise ValueError(f"Initial/static timing {timing!r} cannot set trigger_kind {tk!r} in {wctx}")
        if "condition" in w_spec and w_spec["condition"] not in _ALLOWED_TRIGGER_CONDITIONS:
            raise ValueError(f"Invalid condition {w_spec['condition']!r} in {wctx}")
        if "trigger_kind" in w_spec and w_spec["trigger_kind"] is None:
            if timing in (*_INITIAL_WAVE_TIMINGS, "static_structures") and any(
                k in w_spec for k in ("classification_state", "inference_rule")
            ):
                _validate_state_and_rule(w_spec, wctx)
            elif any(k in w_spec for k in ("classification_state", "inference_rule")):
                raise ValueError(
                    f"Cannot set classification_state/inference_rule when clearing trigger_kind=None on non-initial wave in {wctx}"
                )
        elif any(k in w_spec for k in ("trigger_kind", "classification_state", "inference_rule")):
            _validate_state_and_rule(w_spec, wctx)
        for idx_k in ("source_wave_index", "source_spawn_index", "target_wave_index", "time_seconds"):
            if idx_k in w_spec:
                v = w_spec[idx_k]
                if not isinstance(v, int) or isinstance(v, bool) or v < (0 if idx_k == "time_seconds" else 1):
                    raise ValueError(f"Invalid {idx_k} {v!r} in {wctx}")
        if "target_wave_indices" in w_spec:
            twi = w_spec["target_wave_indices"]
            if not isinstance(twi, list) or not twi or not all(isinstance(x, int) and not isinstance(x, bool) and x >= 1 for x in twi):
                raise ValueError(f"Invalid target_wave_indices {twi!r} in {wctx}")
        if "spawns" in w_spec:
            sp_map = w_spec["spawns"]
            if not isinstance(sp_map, dict):
                raise ValueError(f"Expected dict for spawns in {wctx}")
            for s_key, s_spec in sp_map.items():
                s_idx = _validate_positive_int_key(s_key, f"{wctx}.spawns")
                sctx = f"{wctx}.spawns[{s_idx}]"
                if not isinstance(s_spec, dict):
                    raise ValueError(f"Expected dict in {sctx}")
                bad_sk = set(s_spec.keys()) - allowed_spawn_keys
                if bad_sk:
                    raise ValueError(f"Unknown spawn override keys {sorted(bad_sk)} in {sctx}")
                if "kind" not in s_spec or s_spec["kind"] not in TRIGGER_ENUM:
                    raise ValueError(f"Spawn override requires valid kind in TRIGGER_ENUM in {sctx}")
                if "condition" in s_spec and s_spec["condition"] not in _ALLOWED_TRIGGER_CONDITIONS:
                    raise ValueError(f"Invalid condition {s_spec['condition']!r} in {sctx}")
                _validate_state_and_rule(s_spec, sctx)
                for idx_k in ("target_wave_index", "time_seconds"):
                    if idx_k in s_spec:
                        v = s_spec[idx_k]
                        if not isinstance(v, int) or isinstance(v, bool) or v < (0 if idx_k == "time_seconds" else 1):
                            raise ValueError(f"Invalid {idx_k} {v!r} in {sctx}")
                if "target_wave_indices" in s_spec:
                    twi = s_spec["target_wave_indices"]
                    if not isinstance(twi, list) or not twi or not all(isinstance(x, int) and not isinstance(x, bool) and x >= 1 for x in twi):
                        raise ValueError(f"Invalid target_wave_indices {twi!r} in {sctx}")


def validate_overrides_doc(doc: Any) -> dict[str, Any]:
    """Validate overrides document schema and return normalized reference."""
    if not isinstance(doc, dict):
        raise ValueError("Overrides document must be a JSON object")
    allowed_top = {"schema_version", "description", "notes", "entries"}
    bad_top = set(doc.keys()) - allowed_top
    if bad_top:
        raise ValueError(f"Unknown top-level override keys: {sorted(bad_top)}")
    if doc.get("schema_version") != 1:
        raise ValueError(f"Unsupported overrides schema_version: {doc.get('schema_version')!r} (expected 1)")
    entries = doc.get("entries")
    if not isinstance(entries, dict):
        raise ValueError("Overrides document must contain an 'entries' object")

    allowed_entry_keys = {"objective", "waves", "rooms", "notes"}
    allowed_obj_keys = {
        "kind", "classification_state", "inference_rule", "target_name",
        "target_type_ids", "item_quantity", "volume_m3", "notes",
    }
    for eid, spec in entries.items():
        if not isinstance(eid, str) or not eid.strip():
            raise ValueError(f"Invalid entry_id key in overrides: {eid!r}")
        ectx = f"entries[{eid!r}]"
        if not isinstance(spec, dict):
            raise ValueError(f"Override spec in {ectx} must be a JSON object")
        bad_ek = set(spec.keys()) - allowed_entry_keys
        if bad_ek:
            raise ValueError(f"Unknown entry override keys {sorted(bad_ek)} in {ectx}")
        if "objective" in spec:
            obj = spec["objective"]
            octx = f"{ectx}.objective"
            if not isinstance(obj, dict):
                raise ValueError(f"Objective override in {octx} must be a JSON object")
            bad_ok = set(obj.keys()) - allowed_obj_keys
            if bad_ok:
                raise ValueError(f"Unknown objective override keys {sorted(bad_ok)} in {octx}")
            if obj.get("kind") not in OBJECTIVE_KIND_ENUM:
                raise ValueError(f"Invalid objective kind {obj.get('kind')!r} in {octx}")
            _validate_state_and_rule(obj, octx)
            if "target_type_ids" in obj:
                tids = obj["target_type_ids"]
                if not isinstance(tids, list) or not all(isinstance(t, int) and not isinstance(t, bool) and t > 0 for t in tids):
                    raise ValueError(f"Invalid target_type_ids {tids!r} in {octx}")
            if "target_name" in obj and obj["target_name"] is not None:
                tn = obj["target_name"]
                if not isinstance(tn, str) or not tn.strip():
                    raise ValueError(f"Invalid target_name {tn!r} in {octx}")
            if "item_quantity" in obj and obj["item_quantity"] is not None:
                iq = obj["item_quantity"]
                if not isinstance(iq, int) or isinstance(iq, bool) or iq < 1:
                    raise ValueError(f"Invalid item_quantity {iq!r} in {octx}")
            if "volume_m3" in obj and obj["volume_m3"] is not None:
                vm = obj["volume_m3"]
                if not isinstance(vm, (int, float)) or isinstance(vm, bool) or vm < 0:
                    raise ValueError(f"Invalid volume_m3 {vm!r} in {octx}")
        if "waves" in spec:
            _validate_waves_override_map(spec["waves"], ectx)
        if "rooms" in spec:
            rooms_spec = spec["rooms"]
            if not isinstance(rooms_spec, dict):
                raise ValueError(f"Expected dict for rooms in {ectx}")
            for r_key, r_spec in rooms_spec.items():
                r_idx = _validate_positive_int_key(r_key, f"{ectx}.rooms")
                rctx = f"{ectx}.rooms[{r_idx}]"
                if not isinstance(r_spec, dict) or set(r_spec.keys()) - {"waves", "notes"}:
                    raise ValueError(f"Room override in {rctx} may only contain 'waves'/'notes'")
                if "waves" in r_spec:
                    _validate_waves_override_map(r_spec["waves"], rctx, allow_room_index=False)
    return doc


def load_overrides(path: Path) -> dict[str, Any]:
    """Load and validate structured PvE overrides JSON from disk."""
    doc = json.loads(path.read_text(encoding="utf-8"))
    return validate_overrides_doc(doc)


def apply_overrides(units: list[dict[str, Any]], overrides_doc: dict[str, Any]) -> dict[str, Any]:
    """Deterministically apply post-parse overrides onto parsed units while preserving original_text."""
    validate_overrides_doc(overrides_doc)
    units_by_id: dict[str, dict[str, Any]] = {u["entry_id"]: u for u in units}
    entries_map = overrides_doc.get("entries") or {}

    applied_entries = 0
    applied_objectives = 0
    applied_waves = 0
    applied_spawns = 0
    unmatched_entries: list[str] = []
    unmatched_targets: list[dict[str, Any]] = []

    for eid in sorted(entries_map.keys()):
        unit = units_by_id.get(eid)
        if unit is None:
            unmatched_entries.append(eid)
            continue
        spec = entries_map[eid]
        entry_touched = False

        if "objective" in spec:
            ospec = spec["objective"]
            state, rule = _validate_state_and_rule(ospec, f"entries[{eid!r}].objective")
            orig_text = (unit.get("objective") or {}).get("original_text", "")
            new_obj: dict[str, Any] = {
                "kind": ospec["kind"],
                "classification_state": state,
                "inference_rule": rule,
                "original_text": orig_text,
                "override_applied": True,
            }
            for opt_k in ("target_name", "target_type_ids", "item_quantity", "volume_m3"):
                if opt_k in ospec and ospec[opt_k] is not None:
                    new_obj[opt_k] = sorted(set(ospec[opt_k])) if opt_k == "target_type_ids" else ospec[opt_k]
            unit["objective"] = new_obj
            unit["objective_kind"] = ospec["kind"]
            applied_objectives += 1
            entry_touched = True

        wave_tasks: list[tuple[int, int, dict[str, Any]]] = []
        for w_key, w_spec in (spec.get("waves") or {}).items():
            w_idx = int(w_key)
            r_idx = int(w_spec.get("room_index", 1))
            wave_tasks.append((r_idx, w_idx, w_spec))
        for r_key, r_spec in (spec.get("rooms") or {}).items():
            r_idx = int(r_key)
            for w_key, w_spec in (r_spec.get("waves") or {}).items():
                wave_tasks.append((r_idx, int(w_key), w_spec))

        rooms_by_idx = {r["room_index"]: r for r in unit.get("rooms", [])}
        for r_idx, w_idx, w_spec in sorted(wave_tasks, key=lambda item: (item[0], item[1])):
            room = rooms_by_idx.get(r_idx)
            if room is None:
                unmatched_targets.append({"entry_id": eid, "room_index": r_idx, "wave_index": w_idx, "reason": "missing_room"})
                continue
            waves_by_idx = {w["wave_index"]: w for w in room.get("waves", [])}
            wave = waves_by_idx.get(w_idx)
            if wave is None:
                unmatched_targets.append({"entry_id": eid, "room_index": r_idx, "wave_index": w_idx, "reason": "missing_wave"})
                continue

            if "timing" in w_spec:
                wave["timing"] = w_spec["timing"]

            has_explicit_clear_tkind = "trigger_kind" in w_spec and w_spec["trigger_kind"] is None
            has_trigger_mutation = (
                w_spec.get("trigger_kind") is not None
                or any(
                    k in w_spec
                    for k in ("condition", "time_seconds", "source_wave_index", "source_spawn_index", "target_wave_index", "target_wave_indices")
                )
            )
            if wave["timing"] in (*_INITIAL_WAVE_TIMINGS, "static_structures"):
                if w_spec.get("trigger_kind") is not None:
                    wave["timing"] = "sequential_wave"
                elif has_trigger_mutation:
                    unmatched_targets.append(
                        {"entry_id": eid, "room_index": r_idx, "wave_index": w_idx, "reason": "trigger_field_without_trigger_kind_on_initial_wave"}
                    )
                elif has_explicit_clear_tkind or any(
                    k in w_spec for k in ("timing", "classification_state", "inference_rule")
                ):
                    wave["wave_trigger"] = None
                    rule_ovr = w_spec.get("inference_rule") if w_spec.get("classification_state") == "inferred" else None
                    _refresh_wave_link(wave, rule_ovr)
                    wave["wave_link"]["override_applied"] = True
                    applied_waves += 1
                    entry_touched = True
            if wave["timing"] not in (*_INITIAL_WAVE_TIMINGS, "static_structures"):
                if has_explicit_clear_tkind:
                    wave["wave_trigger"] = None
                    _refresh_wave_link(wave)
                    wave["wave_link"]["override_applied"] = True
                    applied_waves += 1
                    entry_touched = True
                elif has_trigger_mutation or any(
                    k in w_spec for k in ("timing", "classification_state", "inference_rule")
                ):
                    prev_wt = wave.get("wave_trigger") or {}
                    tkind = w_spec["trigger_kind"] if "trigger_kind" in w_spec else prev_wt.get("kind")
                    if tkind is not None:
                        orig_text = prev_wt.get("original_text") or wave["wave_name"]
                        state, rule = _validate_state_and_rule(w_spec, f"{eid}.waves[{w_idx}]")
                        new_wt: dict[str, Any] = {
                            "kind": tkind,
                            "classification_state": state,
                            "inference_rule": rule,
                            "original_text": orig_text,
                            "condition": w_spec.get("condition") or prev_wt.get("condition") or "on_destroy",
                            "target_wave_index": w_spec.get("target_wave_index", w_idx),
                            "override_applied": True,
                        }
                        for opt_k in ("time_seconds", "source_wave_index", "source_spawn_index", "target_wave_indices", "from_wave_header"):
                            val = w_spec.get(opt_k) if opt_k in w_spec else prev_wt.get(opt_k)
                            if val is not None:
                                new_wt[opt_k] = val
                        wave["wave_trigger"] = new_wt
                        _refresh_wave_link(wave)
                    else:
                        if has_trigger_mutation:
                            unmatched_targets.append(
                                {"entry_id": eid, "room_index": r_idx, "wave_index": w_idx, "reason": "missing_trigger_kind"}
                            )
                        rule_ovr = w_spec.get("inference_rule") if w_spec.get("classification_state") == "inferred" else None
                        _refresh_wave_link(wave, rule_ovr)
                    wave["wave_link"]["override_applied"] = True
                    applied_waves += 1
                    entry_touched = True

            for s_key in sorted((w_spec.get("spawns") or {}).keys(), key=int):
                s_idx = int(s_key)
                s_spec = w_spec["spawns"][s_key]
                spawns = wave.get("spawns", [])
                if not (1 <= s_idx <= len(spawns)):
                    unmatched_targets.append(
                        {"entry_id": eid, "room_index": r_idx, "wave_index": w_idx, "spawn_index": s_idx, "reason": "missing_spawn"}
                    )
                    continue
                sp = spawns[s_idx - 1]
                prev_tr = sp.get("trigger") or {}
                orig_text = prev_tr.get("original_text") or wave["wave_name"]
                s_state, s_rule = _validate_state_and_rule(s_spec, f"{eid}.waves[{w_idx}].spawns[{s_idx}]")
                new_tr: dict[str, Any] = {
                    "kind": s_spec["kind"],
                    "classification_state": s_state,
                    "inference_rule": s_rule,
                    "original_text": orig_text,
                    "condition": s_spec.get("condition") or prev_tr.get("condition") or "on_destroy",
                    "override_applied": True,
                }
                if s_spec.get("time_seconds") is not None:
                    new_tr["time_seconds"] = s_spec["time_seconds"]
                ship_meta = _identify_trigger_ship(sp, orig_text, r_idx, w_idx, s_idx)
                new_tr.update(ship_meta)
                targets: list[int] = []
                if "target_wave_indices" in s_spec:
                    targets = list(s_spec["target_wave_indices"])
                    new_tr["target_wave_index"] = targets[0]
                    if len(targets) > 1:
                        new_tr["target_wave_indices"] = targets
                elif "target_wave_index" in s_spec:
                    targets = [s_spec["target_wave_index"]]
                    new_tr["target_wave_index"] = targets[0]
                elif prev_tr.get("target_wave_indices"):
                    targets = list(prev_tr["target_wave_indices"])
                    new_tr["target_wave_index"] = targets[0]
                    if len(targets) > 1:
                        new_tr["target_wave_indices"] = targets
                elif prev_tr.get("target_wave_index") is not None:
                    targets = [prev_tr["target_wave_index"]]
                    new_tr["target_wave_index"] = targets[0]
                sp["trigger"] = new_tr
                for t_idx in targets:
                    tgt_w = waves_by_idx.get(t_idx)
                    if tgt_w is None:
                        unmatched_targets.append(
                            {
                                "entry_id": eid,
                                "room_index": r_idx,
                                "wave_index": w_idx,
                                "spawn_index": s_idx,
                                "target_wave_index": t_idx,
                                "reason": "missing_target_wave",
                            }
                        )
                        continue
                    cur_wt = tgt_w.get("wave_trigger")
                    if (
                        (cur_wt is None or cur_wt.get("classification_state") == "unclassified")
                        and tgt_w["timing"] not in (*_INITIAL_WAVE_TIMINGS, "static_structures")
                    ):
                        wt_payload: dict[str, Any] = {
                            "kind": new_tr["kind"],
                            "classification_state": s_state,
                            "inference_rule": s_rule,
                            "original_text": orig_text,
                            "condition": new_tr["condition"],
                            "source_wave_index": w_idx,
                            "source_spawn_index": s_idx,
                            "target_wave_index": t_idx,
                            "override_applied": True,
                            **ship_meta,
                        }
                        if "time_seconds" in new_tr:
                            wt_payload["time_seconds"] = new_tr["time_seconds"]
                        tgt_w["wave_trigger"] = wt_payload
                        _refresh_wave_link(tgt_w)
                        tgt_w["wave_link"]["override_applied"] = True
                applied_spawns += 1
                entry_touched = True

        if entry_touched:
            applied_entries += 1

    return {
        "entries_in_file": len(entries_map),
        "applied_entries": applied_entries,
        "applied_objectives": applied_objectives,
        "applied_waves": applied_waves,
        "applied_spawns": applied_spawns,
        "unmatched_entries": unmatched_entries,
        "unmatched_targets": unmatched_targets,
    }


def _pct(num: int, den: int) -> float:
    return round(100.0 * num / den, 2) if den > 0 else 0.0


def _state_coverage_block(total: int, explicit_count: int, inferred_count: int, unclassified_count: int, *, classified_label: str = "classified") -> dict[str, Any]:
    classified_count = explicit_count + inferred_count
    return {
        "total": total,
        "explicit_count": explicit_count,
        "explicit_percent": _pct(explicit_count, total),
        "inferred_count": inferred_count,
        "inferred_percent": _pct(inferred_count, total),
        f"{classified_label}_count": classified_count,
        f"{classified_label}_percent": _pct(classified_count, total),
        "unclassified_count": unclassified_count,
        "unclassified_percent": _pct(unclassified_count, total),
    }


def _compute_coverage_metrics(
    all_units: list[dict[str, Any]],
    runtime_entries: list[dict[str, Any]],
    wave_link_state_counts: Counter[str],
    runtime_wave_link_state_counts: Counter[str],
    trigger_state_counts: Counter[str],
    objective_state_counts: Counter[str],
    all_waves_total: int,
    runtime_waves_total: int,
    all_waves_with_dist: int,
    all_waves_unparsed_dist: int,
    npc_total: int,
    npc_resolved_total: int,
    npc_candidate_mapped_total: int,
    npc_partly_resolved: int,
    npc_unresolved_total: int,
) -> dict[str, Any]:
    """Compute structured G1-G5 coverage metrics for report JSON and ratchet checks."""
    spawn_triggers_total = 0
    spawn_triggers_classified = 0
    spawn_triggers_wave_spawning_classified = 0
    spawn_triggers_target_wired = 0
    spawn_triggers_ship_tagged = 0
    mission_units = [u for u in all_units if u.get("unit_kind") == "mission"]
    mission_states: Counter[str] = Counter(u["objective"]["classification_state"] for u in mission_units)

    for u in all_units:
        for r in u.get("rooms", []):
            for w in r.get("waves", []):
                for sp in w.get("spawns", []):
                    tr = sp.get("trigger")
                    if not tr:
                        continue
                    spawn_triggers_total += 1
                    st = tr.get("classification_state")
                    if st != "unclassified":
                        spawn_triggers_classified += 1
                        if tr.get("trigger_ship_tag"):
                            spawn_triggers_ship_tagged += 1
                        if tr.get("kind") != "objective_complete" and not tr.get("is_escalation"):
                            spawn_triggers_wave_spawning_classified += 1
                            if tr.get("target_wave_index") is not None:
                                spawn_triggers_target_wired += 1

    trig_total = sum(trigger_state_counts.values())
    obj_total = len(all_units)
    missions_total = len(mission_units)

    return {
        "g1_triggers": {
            "wave_links": _state_coverage_block(
                all_waves_total,
                wave_link_state_counts.get("explicit", 0),
                wave_link_state_counts.get("inferred", 0),
                wave_link_state_counts.get("unclassified", 0),
            ),
            "runtime_wave_links": _state_coverage_block(
                runtime_waves_total,
                runtime_wave_link_state_counts.get("explicit", 0),
                runtime_wave_link_state_counts.get("inferred", 0),
                runtime_wave_link_state_counts.get("unclassified", 0),
            ),
            "triggers": _state_coverage_block(
                trig_total,
                trigger_state_counts.get("explicit", 0),
                trigger_state_counts.get("inferred", 0),
                trigger_state_counts.get("unclassified", 0),
            ),
            "spawn_triggers_total": spawn_triggers_total,
            "spawn_triggers_classified_count": spawn_triggers_classified,
            "spawn_triggers_wave_spawning_count": spawn_triggers_wave_spawning_classified,
            "spawn_triggers_target_wired_count": spawn_triggers_target_wired,
            "spawn_triggers_target_wired_percent": _pct(spawn_triggers_target_wired, spawn_triggers_wave_spawning_classified),
            "spawn_triggers_ship_tagged_count": spawn_triggers_ship_tagged,
            "spawn_triggers_ship_tagged_percent": _pct(spawn_triggers_ship_tagged, spawn_triggers_classified),
        },
        "g2_objectives": {
            "all_units": _state_coverage_block(
                obj_total,
                objective_state_counts.get("explicit", 0),
                objective_state_counts.get("inferred", 0),
                objective_state_counts.get("unclassified", 0),
                classified_label="structured",
            ),
            "missions": _state_coverage_block(
                missions_total,
                mission_states.get("explicit", 0),
                mission_states.get("inferred", 0),
                mission_states.get("unclassified", 0),
                classified_label="structured",
            ),
        },
        "g3_escalations": {
            "all_units_with_links": sum(1 for u in all_units if u.get("escalation_links")),
            "runtime_units_with_links": sum(1 for u in runtime_entries if u.get("escalation_links")),
            "runtime_escalation_targets_resolved": sum(1 for u in runtime_entries if u.get("escalation_target_ids")),
        },
        "g4_npc_resolution": {
            "total_rows": npc_total,
            "concrete_resolved_count": npc_resolved_total,
            "concrete_resolved_percent": _pct(npc_resolved_total, npc_total),
            "candidate_mapped_count": npc_candidate_mapped_total,
            "candidate_mapped_percent": _pct(npc_candidate_mapped_total, npc_total),
            "partly_resolved_count": npc_partly_resolved,
            "unresolved_count": npc_unresolved_total,
        },
        "g5_wave_distances": {
            "waves_total": all_waves_total,
            "with_distance_m_count": all_waves_with_dist,
            "with_distance_m_percent": _pct(all_waves_with_dist, all_waves_total),
            "unparsed_count": all_waves_unparsed_dist,
        },
    }


_RATCHET_METRIC_PATHS: dict[str, tuple[str, ...]] = {
    "g1_wave_links_classified_percent": ("g1_triggers", "wave_links", "classified_percent"),
    "g1_wave_links_explicit_percent": ("g1_triggers", "wave_links", "explicit_percent"),
    "g1_triggers_classified_percent": ("g1_triggers", "triggers", "classified_percent"),
    "g1_spawn_triggers_target_wired_percent": ("g1_triggers", "spawn_triggers_target_wired_percent"),
    "g2_missions_structured_percent": ("g2_objectives", "missions", "structured_percent"),
    "g2_all_units_structured_percent": ("g2_objectives", "all_units", "structured_percent"),
    "g4_npc_concrete_resolved_percent": ("g4_npc_resolution", "concrete_resolved_percent"),
    "g4_npc_candidate_mapped_percent": ("g4_npc_resolution", "candidate_mapped_percent"),
    "g5_waves_with_distance_m_percent": ("g5_wave_distances", "with_distance_m_percent"),
}


def _extract_ratchet_metric(report: dict[str, Any], metric_name: str) -> float:
    node: Any = report.get("coverage") or {}
    for step in _RATCHET_METRIC_PATHS[metric_name]:
        if not isinstance(node, dict) or step not in node:
            raise KeyError(f"Missing coverage path {'/'.join(_RATCHET_METRIC_PATHS[metric_name])} in report")
        node = node[step]
    return float(node)


def validate_coverage_ratchet_doc(doc: Any) -> dict[str, Any]:
    """Validate coverage ratchet JSON structure and metric floor bounds."""
    if not isinstance(doc, dict):
        raise ValueError("Coverage ratchet document must be a JSON object")
    if doc.get("schema_version") != 1:
        raise ValueError(f"Unsupported coverage ratchet schema_version: {doc.get('schema_version')!r}")
    min_pages = doc.get("min_corpus_pages", 0)
    if not isinstance(min_pages, int) or isinstance(min_pages, bool) or min_pages < 0:
        raise ValueError(f"Invalid min_corpus_pages: {min_pages!r}")
    floors = doc.get("floors")
    if not isinstance(floors, dict) or not floors:
        raise ValueError("Coverage ratchet must contain non-empty 'floors' dict")
    for k, v in floors.items():
        if k not in _RATCHET_METRIC_PATHS:
            raise ValueError(f"Unknown coverage ratchet metric {k!r}")
        if not isinstance(v, (int, float)) or isinstance(v, bool) or not (0.0 <= float(v) <= 100.0):
            raise ValueError(f"Invalid floor value {v!r} for metric {k!r}")
    return doc


def load_coverage_ratchet(path: Path) -> dict[str, Any]:
    """Load and validate coverage ratchet JSON from disk."""
    return validate_coverage_ratchet_doc(json.loads(path.read_text(encoding="utf-8")))


def check_coverage_ratchet(report: dict[str, Any], ratchet_doc: dict[str, Any]) -> list[str]:
    """Return error strings for any coverage metric below its ratchet floor."""
    validate_coverage_ratchet_doc(ratchet_doc)
    errors: list[str] = []
    for metric, floor_val in sorted(ratchet_doc["floors"].items()):
        actual = _extract_ratchet_metric(report, metric)
        if actual < float(floor_val):
            errors.append(f"coverage ratchet regression on {metric}: {actual} < floor {floor_val}")
    return errors


def tighten_coverage_ratchet(
    report: dict[str, Any],
    ratchet_path: Path,
    *,
    min_corpus_pages: int | None = None,
) -> dict[str, Any]:
    """Verify existing ratchet floors pass, then raise floors monotonically to current report metrics."""
    pages_scanned = int((report.get("totals") or {}).get("pages_scanned", 0))
    if ratchet_path.exists():
        doc = load_coverage_ratchet(ratchet_path)
        req_min = int(doc.get("min_corpus_pages", 100)) if min_corpus_pages is None else min_corpus_pages
        if pages_scanned < req_min:
            raise ValueError(
                f"Refusing to tighten coverage ratchet from small corpus: pages_scanned={pages_scanned} < min_corpus_pages={req_min}"
            )
        errs = check_coverage_ratchet(report, doc)
        if errs:
            raise ValueError("; ".join(errs))
    else:
        req_min = 100 if min_corpus_pages is None else min_corpus_pages
        if pages_scanned < req_min:
            raise ValueError(
                f"Refusing to create coverage ratchet from small corpus: pages_scanned={pages_scanned} < min_corpus_pages={req_min}"
            )
        doc = {
            "schema_version": 1,
            "min_corpus_pages": req_min,
            "floors": {k: 0.0 for k in _RATCHET_METRIC_PATHS},
        }
    updated_floors: dict[str, float] = {}
    for metric in sorted(_RATCHET_METRIC_PATHS):
        old_val = float(doc["floors"].get(metric, 0.0))
        cur_val = _extract_ratchet_metric(report, metric)
        updated_floors[metric] = round(max(old_val, cur_val), 2)
    out = {
        "schema_version": 1,
        "description": doc.get("description", "Monotonic coverage percentage floors for wiki PvE importer."),
        "min_corpus_pages": doc.get("min_corpus_pages", req_min),
        "floors": updated_floors,
    }
    ratchet_path.parent.mkdir(parents=True, exist_ok=True)
    ratchet_path.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return out


def run_import(
    wiki_dir: Path,
    sde_db_path: Path,
    mission_ext_path: Path,
    out_content_path: Path,
    out_report_path: Path,
    *,
    overrides_path: Path | None = None,
    overrides_doc: dict[str, Any] | None = None,
    apply_default_overrides: bool = True,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Run full wiki PvE import pipeline and write compact schema_version=2 content + report JSON."""
    sde = SdeIndex(sde_db_path, mission_ext_path)
    articles, catalog_summary = _collect_wiki_articles(wiki_dir)

    active_overrides: dict[str, Any] | None = None
    overrides_source: str | None = None
    if overrides_doc is not None:
        active_overrides = validate_overrides_doc(overrides_doc)
        overrides_source = "<in-memory>"
    elif overrides_path is not None:
        active_overrides = load_overrides(overrides_path)
        overrides_source = str(overrides_path.name)
    elif apply_default_overrides:
        def_ovr = default_overrides_path()
        if def_ovr.exists():
            active_overrides = load_overrides(def_ovr)
            overrides_source = str(def_ovr.name)

    parsed_pages: list[tuple[dict[str, Any], dict[str, Any]]] = []
    all_units: list[dict[str, Any]] = []
    for art in articles:
        text = art["path"].read_text(encoding="utf-8", errors="replace")
        parsed = parse_wiki_page(
            title=art["title"],
            categories=art["categories"],
            wikitext=text,
            sde=sde,
        )
        if parsed is None:
            continue
        parsed_pages.append((art, parsed))
        all_units.extend(parsed["units"])

    if active_overrides is not None:
        override_summary = apply_overrides(all_units, active_overrides)
    else:
        override_summary = {
            "entries_in_file": 0,
            "applied_entries": 0,
            "applied_objectives": 0,
            "applied_waves": 0,
            "applied_spawns": 0,
            "unmatched_entries": [],
            "unmatched_targets": [],
        }
    if overrides_source is not None:
        override_summary["source"] = overrides_source

    for u in all_units:
        for r in u.get("rooms", []):
            for w in r.get("waves", []):
                for sp in w.get("spawns", []):
                    sp.get("resolution", {}).pop("_name_to_type_ids", None)

    status_counts: Counter[str] = Counter()
    trigger_counts: Counter[str] = Counter()
    trigger_state_counts: Counter[str] = Counter()
    wave_link_state_counts: Counter[str] = Counter()
    objective_counts: Counter[str] = Counter()
    objective_state_counts: Counter[str] = Counter()
    partly_resolved_rows: list[dict[str, Any]] = []
    unresolved_rows: list[dict[str, Any]] = []
    unclassified_triggers: list[dict[str, Any]] = []
    unparsed_distances: list[dict[str, Any]] = []
    page_reports: list[dict[str, Any]] = []

    pve_pages_count = len(parsed_pages)
    courier_count = 0
    mining_count = 0
    matched_mission_units = 0
    matched_dungeon_units = 0
    npc_candidate_mapped_total = 0

    all_waves_total = 0
    all_waves_with_dist = 0
    all_waves_unparsed_dist = 0

    for art, parsed in parsed_pages:
        p_rows_tot = 0
        p_rows_resolved = 0
        p_rows_partial = 0
        p_rows_unresolved = 0
        p_trig_unclass = 0
        p_matched_sde = False

        for unit in parsed["units"]:
            eid = unit["entry_id"]
            obj = unit["objective"]
            objective_counts[obj["kind"] or "unclassified"] += 1
            objective_state_counts[obj["classification_state"]] += 1
            if unit["courier"]:
                courier_count += 1
            if unit["mining"]:
                mining_count += 1
            mids = unit["sde_matches"]["mission_ids"]
            dids = unit["sde_matches"]["dungeon_ids"]
            if mids:
                matched_mission_units += 1
                p_matched_sde = True
            if dids:
                matched_dungeon_units += 1
                p_matched_sde = True

            for room in unit["rooms"]:
                for wave in room["waves"]:
                    all_waves_total += 1
                    wlink = wave["wave_link"]
                    wave_link_state_counts[wlink["classification_state"]] += 1
                    if wave.get("distance_m") is not None:
                        all_waves_with_dist += 1
                    elif wave.get("distance_unparsed"):
                        all_waves_unparsed_dist += 1
                        unparsed_distances.append(
                            {
                                "page": art["title"],
                                "entry_id": eid,
                                "room": room["room_name"],
                                "wave": wave["wave_name"],
                                "distance_raw": wave["distance_raw"],
                            }
                        )
                    wtrig = wave.get("wave_trigger")
                    if wtrig:
                        trigger_counts[wtrig["kind"] or "unclassified"] += 1
                        trigger_state_counts[wtrig["classification_state"]] += 1
                        if wtrig["classification_state"] == "unclassified":
                            p_trig_unclass += 1
                            unclassified_triggers.append(
                                {
                                    "page": art["title"],
                                    "entry_id": eid,
                                    "room": room["room_name"],
                                    "wave": wave["wave_name"],
                                    "npc_name": None,
                                    "original_text": wtrig.get("original_text", ""),
                                }
                            )
                    for sp in wave["spawns"]:
                        p_rows_tot += 1
                        res = sp["resolution"]
                        st = res["status"]
                        status_counts[st] += 1
                        if st in ("resolved_exact", "resolved_structure_faction") and res.get("type_ids"):
                            p_rows_resolved += 1
                            npc_candidate_mapped_total += 1
                        elif st == "partly_resolved":
                            p_rows_partial += 1
                            partly_resolved_rows.append(
                                {
                                    "page": art["title"],
                                    "entry_id": eid,
                                    "room": room["room_name"],
                                    "wave": wave["wave_name"],
                                    "raw_class": sp["raw_class"],
                                    "raw_name": sp["raw_name"],
                                    "expanded_names": res["expanded_names"],
                                    "matched_names": res["matched_names"],
                                    "unresolved_parts": res["unresolved_parts"],
                                    "reason": res["reason"],
                                }
                            )
                        else:
                            if st == "resolved_class_candidates" and res.get("candidate_type_ids"):
                                npc_candidate_mapped_total += 1
                            p_rows_unresolved += 1
                            unres_entry: dict[str, Any] = {
                                "page": art["title"],
                                "entry_id": eid,
                                "room": room["room_name"],
                                "wave": wave["wave_name"],
                                "raw_class": sp["raw_class"],
                                "raw_name": sp["raw_name"],
                                "factions": unit["factions"],
                                "status": st,
                                "reason": res["reason"] or f"unresolved_status:{st}",
                            }
                            if res.get("candidate_type_ids"):
                                unres_entry["candidate_type_ids"] = res["candidate_type_ids"]
                            unresolved_rows.append(unres_entry)

                        tr = sp.get("trigger")
                        if tr:
                            trigger_counts[tr["kind"] or "unclassified"] += 1
                            trigger_state_counts[tr["classification_state"]] += 1
                            if tr["classification_state"] == "unclassified":
                                p_trig_unclass += 1
                                unclassified_triggers.append(
                                    {
                                        "page": art["title"],
                                        "entry_id": eid,
                                        "room": room["room_name"],
                                        "wave": wave["wave_name"],
                                        "npc_name": sp["raw_name"],
                                        "original_text": tr.get("original_text", ""),
                                    }
                                )

        page_reports.append(
            {
                "page_title": art["title"],
                "categories": sorted(art["categories"]),
                "units_count": len(parsed["units"]),
                "matched_sde": p_matched_sde,
                "npc_rows_total": p_rows_tot,
                "npc_rows_resolved": p_rows_resolved,
                "npc_rows_partly_resolved": p_rows_partial,
                "npc_rows_unresolved": p_rows_unresolved,
                "trigger_unclassified_count": p_trig_unclass,
            }
        )

    # G3: Segregate unmatched records out of runtime content JSON into report JSON BEFORE G4 linking
    matched_units = [u for u in all_units if u["sde_matches"]["mission_ids"] or u["sde_matches"]["dungeon_ids"]]
    unmatched_units = [u for u in all_units if not u["sde_matches"]["mission_ids"] and not u["sde_matches"]["dungeon_ids"]]

    # G4: Wire expedition & multi-part escalation chains strictly across matched runtime entries
    _wire_escalation_chains(matched_units)
    indexes = _build_disambiguated_indexes(matched_units)

    # G5: Compact runtime entries and export stripped metadata prose to report JSON
    runtime_entries: list[dict[str, Any]] = []
    entry_metadata_prose: list[dict[str, Any]] = []
    for u in matched_units:
        compact_u, prose = _compact_entry_for_runtime(u)
        runtime_entries.append(compact_u)
        if prose:
            entry_metadata_prose.append(prose)

    runtime_waves_total = 0
    runtime_waves_with_dist = 0
    runtime_waves_unparsed_dist = 0
    runtime_waves_unclassified = 0
    runtime_wave_link_state_counts: Counter[str] = Counter()
    for u in runtime_entries:
        for r in u["rooms"]:
            for w in r["waves"]:
                runtime_waves_total += 1
                if w.get("distance_m") is not None:
                    runtime_waves_with_dist += 1
                elif w.get("distance_unparsed"):
                    runtime_waves_unparsed_dist += 1
                w_st = w["wave_link"]["classification_state"]
                runtime_wave_link_state_counts[w_st] += 1
                if w_st == "unclassified":
                    runtime_waves_unclassified += 1

    npc_total = sum(status_counts.values())
    npc_resolved_total = status_counts.get("resolved_exact", 0) + status_counts.get("resolved_structure_faction", 0)
    npc_unresolved_total = len(unresolved_rows)
    resolved_pct = round(100.0 * npc_resolved_total / npc_total, 2) if npc_total else 0.0
    candidate_mapped_pct = round(100.0 * npc_candidate_mapped_total / npc_total, 2) if npc_total else 0.0

    coverage = _compute_coverage_metrics(
        all_units=all_units,
        runtime_entries=runtime_entries,
        wave_link_state_counts=wave_link_state_counts,
        runtime_wave_link_state_counts=runtime_wave_link_state_counts,
        trigger_state_counts=trigger_state_counts,
        objective_state_counts=objective_state_counts,
        all_waves_total=all_waves_total,
        runtime_waves_total=runtime_waves_total,
        all_waves_with_dist=all_waves_with_dist,
        all_waves_unparsed_dist=all_waves_unparsed_dist,
        npc_total=npc_total,
        npc_resolved_total=npc_resolved_total,
        npc_candidate_mapped_total=npc_candidate_mapped_total,
        npc_partly_resolved=status_counts.get("partly_resolved", 0),
        npc_unresolved_total=npc_unresolved_total,
    )

    header = {
        "schema_version": 2,
        "source": "EVE University Wiki (~/ai/eve-wiki) + local client SDE",
        "license": "CC BY-SA 4.0 (locally generated from wiki copy; never commit to git)",
        "wiki_catalog_summary": catalog_summary,
        "deterministic": True,
    }

    content_doc: dict[str, Any] = {
        "header": header,
        "entries": runtime_entries,
        "indexes": indexes,
    }

    report_doc: dict[str, Any] = {
        "header": header,
        "overrides": override_summary,
        "coverage": coverage,
        "totals": {
            "pages_scanned": len(articles),
            "pve_pages_extracted": pve_pages_count,
            "content_units": len(all_units),
            "runtime_content_units": len(runtime_entries),
            "unmatched_units": len(unmatched_units),
            "units_matched_to_sde_mission": matched_mission_units,
            "units_matched_to_sde_dungeon": matched_dungeon_units,
            "mission_primary_keys": len(indexes["by_mission_key"]),
            "dungeon_primary_keys": len(indexes["by_dungeon_key"]),
            "units_with_courier_payload": courier_count,
            "units_with_mining_payload": mining_count,
            "units_with_escalation_links": sum(1 for u in runtime_entries if u.get("escalation_links")),
            "objective_classification_counts": {k: objective_counts[k] for k in sorted(objective_counts)},
            "objective_classification_state_counts": {
                k: objective_state_counts[k] for k in sorted(objective_state_counts)
            },
            "waves_total": all_waves_total,
            "waves_with_distance_m": all_waves_with_dist,
            "waves_with_unparsed_distance": all_waves_unparsed_dist,
            "unparsed_distances_total": len(unparsed_distances),
            "wave_link_classification_state_counts": {
                k: wave_link_state_counts[k] for k in sorted(wave_link_state_counts)
            },
            "runtime_waves_total": runtime_waves_total,
            "runtime_waves_with_distance_m": runtime_waves_with_dist,
            "runtime_waves_with_unparsed_distance": runtime_waves_unparsed_dist,
            "runtime_waves_unclassified": runtime_waves_unclassified,
            "runtime_wave_link_classification_state_counts": {
                k: runtime_wave_link_state_counts[k] for k in sorted(runtime_wave_link_state_counts)
            },
            "npc_rows_total": npc_total,
            "npc_rows_resolved_total": npc_resolved_total,
            "npc_rows_resolved_percent": resolved_pct,
            "npc_rows_candidate_mapped_total": npc_candidate_mapped_total,
            "npc_rows_candidate_mapped_percent": candidate_mapped_pct,
            "npc_rows_partly_resolved": status_counts.get("partly_resolved", 0),
            "npc_rows_unresolved": npc_unresolved_total,
            "npc_resolution_breakdown": {k: status_counts[k] for k in sorted(status_counts)},
            "trigger_total": sum(trigger_counts.values()),
            "trigger_classification_counts": {k: trigger_counts[k] for k in sorted(trigger_counts)},
            "trigger_classification_state_counts": {
                k: trigger_state_counts[k] for k in sorted(trigger_state_counts)
            },
            "trigger_unclassified_count": len(unclassified_triggers),
        },
        "unclassified_triggers": unclassified_triggers,
        "unparsed_distances": unparsed_distances,
        "partly_resolved_rows": partly_resolved_rows,
        "unresolved_rows": unresolved_rows,
        "unmatched_entries": unmatched_units,
        "entry_metadata_prose": entry_metadata_prose,
        "pages": page_reports,
    }

    out_content_path.parent.mkdir(parents=True, exist_ok=True)
    out_report_path.parent.mkdir(parents=True, exist_ok=True)
    out_content_path.write_text(
        json.dumps(content_doc, separators=(",", ":"), sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    out_report_path.write_text(
        json.dumps(report_doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return content_doc, report_doc


def _verify_three_state_trigger(tr: dict[str, Any], ctx: str, errors: list[str]) -> None:
    st = tr.get("classification_state")
    if st not in CLASSIFICATION_STATES:
        errors.append(f"invalid trigger classification_state {st!r} in {ctx}")
        return
    if not isinstance(tr.get("original_text"), str) or not tr.get("original_text"):
        errors.append(f"trigger missing non-empty original_text in {ctx}")
    kind = tr.get("kind")
    if st == "unclassified":
        if kind is not None or tr.get("inference_rule") is not None:
            errors.append(f"unclassified trigger must not carry kind/rule in {ctx}: {tr}")
    else:
        if kind not in TRIGGER_ENUM:
            errors.append(f"classified trigger invalid kind {kind!r} in {ctx}")
        if st == "inferred" and not tr.get("inference_rule"):
            errors.append(f"inferred trigger missing inference_rule in {ctx}")
        if st == "explicit" and tr.get("inference_rule") is not None:
            errors.append(f"explicit trigger has unexpected inference_rule in {ctx}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Import EVE University wiki PvE missions and combat sites into JSON."
    )
    parser.add_argument(
        "--wiki-dir",
        type=Path,
        default=default_wiki_dir(),
        help="Path to local EVE University wiki directory (default: ~/ai/eve-wiki)",
    )
    parser.add_argument(
        "--sde-db",
        type=Path,
        default=default_sde_db(),
        help="Path to local evomu_sde.sqlite database",
    )
    parser.add_argument(
        "--mission-ext",
        type=Path,
        default=default_mission_ext(),
        help="Path to data/sde_agent_mission_ext.json",
    )
    parser.add_argument(
        "--out-content",
        type=Path,
        default=REPO_ROOT / "data" / "wiki_pve_content.json",
        help="Output path for generated PvE content JSON (gitignored)",
    )
    parser.add_argument(
        "--out-report",
        type=Path,
        default=REPO_ROOT / "data" / "wiki_pve_report.json",
        help="Output path for generated coverage report JSON (gitignored)",
    )
    parser.add_argument(
        "--overrides",
        type=Path,
        default=None,
        help="Path to structured post-parse overrides JSON (default: tools/wiki_pve_overrides.json if present)",
    )
    parser.add_argument(
        "--no-overrides",
        action="store_true",
        help="Disable post-parse overrides application",
    )
    parser.add_argument(
        "--coverage-ratchet",
        type=Path,
        default=None,
        help="Path to coverage ratchet JSON (default: tools/wiki_pve_coverage_ratchet.json if present on full corpus)",
    )
    parser.add_argument(
        "--no-coverage-ratchet",
        action="store_true",
        help="Disable coverage ratchet check during --verify",
    )
    parser.add_argument(
        "--tighten-ratchet",
        action="store_true",
        help="Raise coverage ratchet floors monotonically to current run metrics without lowering existing floors",
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="Run import twice and verify deterministic SHA-256 hashes, accounting invariants, and coverage thresholds",
    )
    parser.add_argument(
        "--min-resolved-percent",
        type=float,
        default=85.0,
        help=(
            "Minimum strictly-concrete NPC row resolved percentage enforced by --verify "
            "(default: 85.0; empirical ~/ai/eve-wiki sde_types concrete ceiling ~87.1%% "
            "due to 293 blank cells + 201 bare prefix/? + 994 CCP custom dungeon names)"
        ),
    )
    parser.add_argument(
        "--min-candidate-mapped-percent",
        type=float,
        default=90.0,
        help=(
            "Minimum NPC row percentage with concrete type_ids plus faction+class candidate rows "
            "(resolved_class_candidates with non-empty candidate_type_ids) enforced by --verify "
            "(default: 90.0)"
        ),
    )
    args = parser.parse_args(argv)

    ovr_path = None if args.no_overrides else args.overrides
    apply_def_ovr = not args.no_overrides
    content_doc, report = run_import(
        wiki_dir=args.wiki_dir,
        sde_db_path=args.sde_db,
        mission_ext_path=args.mission_ext,
        out_content_path=args.out_content,
        out_report_path=args.out_report,
        overrides_path=ovr_path,
        apply_default_overrides=apply_def_ovr,
    )
    totals = report["totals"]
    cov = report["coverage"]
    print(
        f"Imported {totals['content_units']} PvE units ({totals['runtime_content_units']} runtime, "
        f"{totals['unmatched_units']} unmatched) across {totals['pve_pages_extracted']} pages "
        f"(missions_matched={totals['units_matched_to_sde_mission']}, "
        f"dungeons_matched={totals['units_matched_to_sde_dungeon']}); "
        f"NPC rows resolved (concrete type_ids): {totals['npc_rows_resolved_total']}/{totals['npc_rows_total']} "
        f"({totals['npc_rows_resolved_percent']}%); "
        f"candidate_mapped: {totals['npc_rows_candidate_mapped_total']}/{totals['npc_rows_total']} "
        f"({totals['npc_rows_candidate_mapped_percent']}%); "
        f"wave_links_classified={cov['g1_triggers']['wave_links']['classified_percent']}%, "
        f"missions_structured={cov['g2_objectives']['missions']['structured_percent']}%, "
        f"unclassified_triggers={totals['trigger_unclassified_count']}; "
        f"unparsed_distances={totals['unparsed_distances_total']}"
    )

    if args.tighten_ratchet:
        r_target = args.coverage_ratchet or default_coverage_ratchet_path()
        try:
            updated = tighten_coverage_ratchet(report, r_target)
        except ValueError as exc:
            print(f"TIGHTEN FAIL: {exc}", file=sys.stderr)
            return 1
        print(f"Tightened coverage ratchet in {r_target}: {updated['floors']}")

    if args.verify:
        with tempfile.TemporaryDirectory() as tmpdir:
            tmp_root = Path(tmpdir)
            out_content_2 = tmp_root / "wiki_pve_content.json"
            out_report_2 = tmp_root / "wiki_pve_report.json"
            run_import(
                wiki_dir=args.wiki_dir,
                sde_db_path=args.sde_db,
                mission_ext_path=args.mission_ext,
                out_content_path=out_content_2,
                out_report_path=out_report_2,
                overrides_path=ovr_path,
                apply_default_overrides=apply_def_ovr,
            )
            sha_c1 = hashlib.sha256(args.out_content.read_bytes()).hexdigest()
            sha_c2 = hashlib.sha256(out_content_2.read_bytes()).hexdigest()
            sha_r1 = hashlib.sha256(args.out_report.read_bytes()).hexdigest()
            sha_r2 = hashlib.sha256(out_report_2.read_bytes()).hexdigest()

        errors: list[str] = []
        if content_doc.get("header", {}).get("schema_version") != 2:
            errors.append(f"unexpected schema_version: {content_doc.get('header', {}).get('schema_version')}")
        if sha_c1 != sha_c2:
            errors.append(f"non-deterministic content JSON: {sha_c1} != {sha_c2}")
        if sha_r1 != sha_r2:
            errors.append(f"non-deterministic report JSON: {sha_r1} != {sha_r2}")
        if totals["content_units"] <= 0:
            errors.append("no PvE content units extracted")
        if totals["npc_rows_total"] <= 0:
            errors.append("no NPC rows extracted")
        if totals["npc_rows_resolved_percent"] < args.min_resolved_percent:
            errors.append(
                f"NPC concrete resolved percent {totals['npc_rows_resolved_percent']}% < {args.min_resolved_percent}%"
            )
        if totals["npc_rows_candidate_mapped_percent"] < args.min_candidate_mapped_percent:
            errors.append(
                f"NPC candidate-mapped percent {totals['npc_rows_candidate_mapped_percent']}% < {args.min_candidate_mapped_percent}%"
            )
        if totals["npc_rows_resolved_total"] + totals["npc_rows_partly_resolved"] + totals["npc_rows_unresolved"] != totals["npc_rows_total"]:
            errors.append("npc row totals accounting mismatch")
        if totals["npc_rows_partly_resolved"] != len(report["partly_resolved_rows"]):
            errors.append("partly_resolved_rows accounting mismatch")
        if totals["npc_rows_unresolved"] != len(report["unresolved_rows"]):
            errors.append("unresolved_rows accounting mismatch")
        if totals["trigger_unclassified_count"] != len(report["unclassified_triggers"]):
            errors.append("unclassified_triggers accounting mismatch")
        if totals["unparsed_distances_total"] != len(report["unparsed_distances"]):
            errors.append("unparsed_distances accounting mismatch")
        if totals["waves_with_unparsed_distance"] != totals["unparsed_distances_total"]:
            errors.append("waves_with_unparsed_distance accounting mismatch")
        if sum(totals["wave_link_classification_state_counts"].values()) != totals["waves_total"]:
            errors.append("wave_link_classification_state_counts accounting mismatch")
        if sum(totals["runtime_wave_link_classification_state_counts"].values()) != totals["runtime_waves_total"]:
            errors.append("runtime_wave_link_classification_state_counts accounting mismatch")
        if totals["runtime_wave_link_classification_state_counts"].get("unclassified", 0) != totals["runtime_waves_unclassified"]:
            errors.append("runtime_waves_unclassified accounting mismatch")
        if totals["unmatched_units"] != len(report["unmatched_entries"]):
            errors.append("unmatched_entries accounting mismatch")
        if totals["runtime_content_units"] != len(content_doc["entries"]):
            errors.append("runtime_content_units accounting mismatch")
        if cov["g1_triggers"]["wave_links"]["total"] != totals["waves_total"]:
            errors.append("coverage wave_links total accounting mismatch")
        if cov["g2_objectives"]["all_units"]["total"] != totals["content_units"]:
            errors.append("coverage objectives total accounting mismatch")
        if sum(totals["objective_classification_state_counts"].values()) != totals["content_units"]:
            errors.append("objective_classification_state_counts accounting mismatch")

        ratchet_path = None
        min_pages_for_stale = 100
        if not args.no_coverage_ratchet:
            if args.coverage_ratchet is not None:
                ratchet_path = args.coverage_ratchet
                r_doc = load_coverage_ratchet(ratchet_path)
                min_pages_for_stale = int(r_doc.get("min_corpus_pages", 100))
                if totals["pages_scanned"] < min_pages_for_stale:
                    errors.append(
                        f"pages_scanned {totals['pages_scanned']} < coverage ratchet min_corpus_pages {min_pages_for_stale}"
                    )
                else:
                    errors.extend(check_coverage_ratchet(report, r_doc))
            elif default_coverage_ratchet_path().exists():
                ratchet_path = default_coverage_ratchet_path()
                r_doc = load_coverage_ratchet(ratchet_path)
                min_pages_for_stale = int(r_doc.get("min_corpus_pages", 100))
                if totals["pages_scanned"] >= min_pages_for_stale:
                    errors.extend(check_coverage_ratchet(report, r_doc))

        if totals["pages_scanned"] >= min_pages_for_stale or (args.overrides is not None and not args.no_overrides):
            if report["overrides"].get("unmatched_targets"):
                errors.append(f"stale override targets: {report['overrides']['unmatched_targets']}")
            if report["overrides"].get("unmatched_entries"):
                errors.append(f"stale override entries: {report['overrides']['unmatched_entries']}")

        verified_concrete = 0
        verified_candidate_mapped = 0
        runtime_entry_ids: set[str] = set()
        for u in content_doc["entries"]:
            runtime_entry_ids.add(u["entry_id"])
            mids = u["sde_matches"]["mission_ids"]
            dids = u["sde_matches"]["dungeon_ids"]
            if not mids and not dids:
                errors.append(f"unmatched entry leaked into runtime content: {u['entry_id']}")

            obj = u.get("objective")
            if not isinstance(obj, dict) or obj.get("classification_state") not in CLASSIFICATION_STATES:
                errors.append(f"invalid objective slot in {u['entry_id']}: {obj}")
            else:
                ost = obj["classification_state"]
                okind = obj.get("kind")
                if ost == "unclassified":
                    if okind is not None or obj.get("inference_rule") is not None:
                        errors.append(f"unclassified objective must not carry kind/rule in {u['entry_id']}")
                else:
                    if okind not in OBJECTIVE_KIND_ENUM:
                        errors.append(f"invalid objective kind in {u['entry_id']}: {okind}")
                    if ost == "inferred" and not obj.get("inference_rule"):
                        errors.append(f"inferred objective missing inference_rule in {u['entry_id']}")
                    if ost == "explicit" and obj.get("inference_rule") is not None:
                        errors.append(f"explicit objective has unexpected inference_rule in {u['entry_id']}")
                if not isinstance(obj.get("original_text"), str):
                    errors.append(f"objective missing original_text string in {u['entry_id']}")
                if u.get("objective_kind") != okind:
                    errors.append(f"objective_kind mismatch in {u['entry_id']}")

            for r in u["rooms"]:
                for w in r["waves"]:
                    wctx = f"{u['entry_id']} / {r['room_name']} / {w['wave_name']}"
                    if not w["spawns"]:
                        errors.append(f"empty ghost wave in {wctx}")
                    wlink = w.get("wave_link")
                    if not isinstance(wlink, dict) or wlink.get("classification_state") not in CLASSIFICATION_STATES:
                        errors.append(f"invalid wave_link in {wctx}: {wlink}")
                    else:
                        wst = wlink["classification_state"]
                        if wst == "inferred" and not wlink.get("inference_rule"):
                            errors.append(f"inferred wave_link missing inference_rule in {wctx}")
                        if wst == "explicit" and wlink.get("inference_rule") is not None:
                            errors.append(f"explicit wave_link has unexpected inference_rule in {wctx}")
                        if wst == "unclassified" and (wlink.get("inference_rule") is not None or wlink.get("trigger_kind") is not None):
                            errors.append(f"unclassified wave_link has non-null rule/trigger_kind in {wctx}")
                        if not isinstance(wlink.get("original_text"), str):
                            errors.append(f"wave_link missing original_text in {wctx}")
                    if w.get("wave_trigger") is not None:
                        _verify_three_state_trigger(w["wave_trigger"], wctx, errors)
                    if (
                        w["timing"] in ("initial_spawn", "static_structures", "simultaneous_initial_group")
                        and w.get("wave_trigger") is not None
                    ):
                        errors.append(f"{w['timing']} has non-null wave_trigger in {wctx}")
                    if w.get("distance_unparsed"):
                        if w.get("distance_m") is not None or not w.get("distance_raw"):
                            errors.append(f"invalid distance_unparsed flags in {wctx}")
                    for sp in w["spawns"]:
                        if sp.get("trigger") is not None:
                            _verify_three_state_trigger(sp["trigger"], f"{wctx} / {sp['raw_name']}", errors)
                        sres = sp["resolution"]
                        if sres["status"] in ("resolved_exact", "resolved_structure_faction"):
                            if not sres.get("type_ids"):
                                errors.append(f"resolved status without concrete type_ids in {u['entry_id']}: {sp['raw_name']}")
                            else:
                                verified_concrete += 1
                                verified_candidate_mapped += 1
                        elif sres["status"] == "resolved_class_candidates" and sres.get("candidate_type_ids"):
                            verified_candidate_mapped += 1

        for u in report.get("unmatched_entries", []):
            for r in u["rooms"]:
                for w in r["waves"]:
                    for sp in w["spawns"]:
                        sres = sp["resolution"]
                        if sres["status"] in ("resolved_exact", "resolved_structure_faction") and sres.get("type_ids"):
                            verified_concrete += 1
                            verified_candidate_mapped += 1
                        elif sres["status"] == "resolved_class_candidates" and sres.get("candidate_type_ids"):
                            verified_candidate_mapped += 1

        for u in content_doc["entries"]:
            nxt = u.get("next_stage_entry_id")
            if nxt is not None and (nxt not in runtime_entry_ids or nxt == u["entry_id"]):
                errors.append(f"dangling or self next_stage_entry_id in {u['entry_id']}: {nxt}")
            for tid in u.get("escalation_target_ids", []):
                if tid not in runtime_entry_ids or tid == u["entry_id"]:
                    errors.append(f"dangling or self escalation_target_id in {u['entry_id']}: {tid}")

        for idx_name in ("by_mission_key", "by_dungeon_key"):
            idx_map = content_doc["indexes"].get(idx_name, {})
            for pk, eid in idx_map.items():
                if not isinstance(eid, str) or eid not in runtime_entry_ids:
                    errors.append(f"dangling or non-1to1 key {pk} in {idx_name}: {eid}")
                    break

        if verified_concrete != totals["npc_rows_resolved_total"]:
            errors.append("concrete resolved count accounting mismatch")
        if verified_candidate_mapped != totals["npc_rows_candidate_mapped_total"]:
            errors.append("candidate-mapped count accounting mismatch")
        if errors:
            for err in errors:
                print(f"VERIFY FAIL: {err}", file=sys.stderr)
            return 1
        print(
            f"VERIFY PASS: schema_v2 runtime={totals['runtime_content_units']} "
            f"unmatched={totals['unmatched_units']} "
            f"concrete_resolved={totals['npc_rows_resolved_total']}/{totals['npc_rows_total']} "
            f"({totals['npc_rows_resolved_percent']}% >= {args.min_resolved_percent}%) "
            f"candidate_mapped={totals['npc_rows_candidate_mapped_total']}/{totals['npc_rows_total']} "
            f"({totals['npc_rows_candidate_mapped_percent']}% >= {args.min_candidate_mapped_percent}%) "
            f"content_sha256={sha_c1} report_sha256={sha_r1}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
