#!/usr/bin/env python3
"""Import EVE University wiki PvE missions and combat sites into deterministic JSON.

Reads local MediaWiki wikitext pages from ~/ai/eve-wiki (or --wiki-dir), expands
NPCTableRow / NPCTableHead / NPCTableSeparator / Missiondetails / CMBSite* templates,
matches NPCs, structures, containers, asteroids, mission items, and dungeons against the
local SDE SQLite database + sde_agent_mission_ext.json, classifies triggers and EWAR,
and writes gitignored data/wiki_pve_content.json and data/wiki_pve_report.json.

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

ALLOWED_SDE_CATEGORIES = (2, 3, 6, 11, 18, 22, 23, 25, 40, 46, 65, 87)

PVE_CATEGORIES = {
    "Security missions",
    "Distribution Missions",
    "Mining Missions",
    "COSMOS missions",
    "COSMOS sites",
    "Epic arc missions",
    "Storyline missions",
    "Combat Anomalies",
    "DED Complexes",
    "Unrated Complexes",
    "Expeditions",
    "Chemical Labs",
    "Relic sites",
    "Data sites",
    "Gas sites",
    "Ore sites",
    "Wormhole sites",
    "Anomic missions",
    "Career Agents",
    "Mercenary Tactical Operations",
    "Data center missions",
}

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

NAME_TO_CANON_FACTION: dict[str, str] = {
    "guristas": "Guristas Pirates",
    "pith": "Guristas Pirates",
    "angel": "Angel Cartel",
    "arch angel": "Angel Cartel",
    "domination": "Angel Cartel",
    "gist": "Angel Cartel",
    "blood": "Blood Raider Covenant",
    "corpus": "Blood Raider Covenant",
    "corpior": "Blood Raider Covenant",
    "corpatis": "Blood Raider Covenant",
    "corpum": "Blood Raider Covenant",
    "corpii": "Blood Raider Covenant",
    "sansha": "Sansha's Nation",
    "centi": "Sansha's Nation",
    "centus": "Sansha's Nation",
    "centum": "Sansha's Nation",
    "centior": "Sansha's Nation",
    "centatis": "Sansha's Nation",
    "serpentis": "Serpentis",
    "core": "Serpentis",
    "corelum": "Serpentis",
    "corelior": "Serpentis",
    "coreli": "Serpentis",
    "corelatis": "Serpentis",
    "rogue drone": "Rogue Drones",
    "alvi": "Rogue Drones",
    "alvu": "Rogue Drones",
    "alvatis": "Rogue Drones",
    "amarr": "Amarr Empire",
    "imperial": "Amarr Empire",
    "caldari": "Caldari State",
    "state ": "Caldari State",
    "gallente": "Gallente Federation",
    "federation": "Gallente Federation",
    "minmatar": "Minmatar Republic",
    "republic": "Minmatar Republic",
    "mordu": "Mordu's Legion Command",
    "mercenar": "Mercenaries",
    "khanid": "Khanid Kingdom",
    "ammatar": "Ammatar Mandate",
    "thukker": "Thukker Tribe",
    "syndicate": "The Syndicate",
    "concord": "CONCORD Assembly",
    "triglavian": "Triglavian Collective",
    "edencom": "EDENCOM",
    "drifter": "Drifters",
    "eom": "Equilibrium of Mankind",
    "equilibrium of mankind": "Equilibrium of Mankind",
}

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
    ("centi ", "centii "),
    ("gisti ", "gistii "),
    ("war lord", "warlord"),
    ("sentinal", "sentinel"),
    ("infilitrator", "infiltrator"),
    ("mahariel", "machariel"),
    ("watchmen", "watchman"),
    ("archangels ", "arch angel "),
    ("archangel ", "arch angel "),
    ("archangel", "arch angel"),
    ("blood raider ", "bloodraider "),
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

FAMILY_RANK_SIBLINGS: dict[str, list[str]] = {
    "corpii": ["corpii", "corpior", "corpum", "corpatis", "corpus", "dark corpii", "dark corpior", "dark corpum", "dark corpatis", "dark corpus", "elder corpii", "elder corpum"],
    "corpior": ["corpii", "corpior", "corpum", "corpatis", "corpus", "dark corpior", "dark corpatis", "dark corpus"],
    "corpum": ["corpii", "corpior", "corpum", "corpatis", "corpus", "dark corpum"],
    "corpatis": ["corpii", "corpior", "corpum", "corpatis", "corpus", "dark corpatis"],
    "corpus": ["corpii", "corpior", "corpum", "corpatis", "corpus", "dark corpus"],
    "gistii": ["gistii", "gistior", "gistum", "gistatis", "gist", "arch gistii"],
    "gistior": ["gistii", "gistior", "gistum", "gistatis", "gist"],
    "gistum": ["gistii", "gistior", "gistum", "gistatis", "gist", "arch gistum"],
    "gistatis": ["gistii", "gistior", "gistum", "gistatis", "gist"],
    "gist": ["gistii", "gistior", "gistum", "gistatis", "gist"],
    "coreli": ["coreli", "corelior", "corelum", "corelatis", "core", "shadow coreli", "shadow corelior", "shadow corelum"],
    "corelior": ["coreli", "corelior", "corelum", "corelatis", "core"],
    "corelum": ["coreli", "corelior", "corelum", "corelatis", "core", "shadow corelum"],
    "corelatis": ["coreli", "corelior", "corelum", "corelatis", "core"],
    "core": ["coreli", "corelior", "corelum", "corelatis", "core"],
    "centii": ["centii", "centior", "centum", "centatis", "centus", "true centii", "true centum", "true centus"],
    "centior": ["centii", "centior", "centum", "centatis", "centus"],
    "centum": ["centii", "centior", "centum", "centatis", "centus"],
    "centatis": ["centii", "centior", "centum", "centatis", "centus"],
    "centus": ["centii", "centior", "centum", "centatis", "centus"],
    "pithi": ["pithi", "pithior", "pithum", "pithatis", "pith", "dire pithi", "dread pithi"],
    "pithior": ["pithi", "pithior", "pithum", "pithatis", "pith"],
    "pithum": ["pithi", "pithior", "pithum", "pithatis", "pith"],
    "pithatis": ["pithi", "pithior", "pithum", "pithatis", "pith"],
    "pith": ["pithi", "pithior", "pithum", "pithatis", "pith"],
    "imperial": ["imperial", "imperial templar", "divine imperial", "imperial navy", "amarr navy"],
    "divine": ["divine imperial", "imperial"],
    "blood": ["blood", "dark blood", "elder blood", "corpii", "corpior", "corpum", "corpatis", "corpus"],
    "strain": ["strain"],
    "rogue": ["rogue", "rogue pirate", "rogue drone"],
    "angel": ["angel", "arch angel", "domination", "gistii", "gistior", "gistum", "gistatis", "gist"],
    "serpentis": ["serpentis", "shadow serpentis", "coreli", "corelior", "corelum", "corelatis", "core"],
    "guristas": ["guristas", "dread guristas", "pithi", "pithior", "pithum", "pithatis", "pith"],
    "sansha": ["sansha", "sansha's", "true sansha", "centii", "centior", "centum", "centatis", "centus"],
}

STRUCTURE_ALIASES: dict[str, list[str]] = {
    "stasis tower": ["{fac} Stasis Tower", "Stasis Webification Battery", "Pirate Stasis Tower"],
    "starbase stasis tower": ["{fac} Stasis Tower", "Stasis Webification Battery"],
    "bunker": ["{fac} Bunker", "Occupied {fac} Bunker", "Drone Bunker"],
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
    "energy neutralizer sentry": ["{fac} Energy Neutralizer Sentry I", "{fac} Energy Neutralizer Sentry II", "{fac} Energy Neutralizer Sentry"],
    "tower sentry": ["Tower Sentry {fac} I", "Tower Sentry {fac} II", "Tower Sentry {fac} III", "{fac} Sentry Gun"],
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
    "power generator": ["{fac} Power Generator", "Power Generator"],
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
    r"\b(?:pocket|room|stage|chamber|hall|part\s*\d+|area|zone)\b|"
    r"\b(?:first|second|third|fourth|fifth|1st|2nd|3rd|4th|5th|initial|final|last|gated)\s+(?:pocket|room|area|part|stage)\b",
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

TRIGGER_ENUM = (
    "this_ship_destroyed",
    "last_in_group_destroyed",
    "last_of_class_destroyed",
    "ship_approached",
    "timed",
    "objective_complete",
    "group_destroyed",
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


def infer_faction_from_text(text: str) -> str | None:
    low = text.lower()
    for key, canon in NAME_TO_CANON_FACTION.items():
        if key in low:
            return canon
    return None


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
        fac = infer_faction_from_text(q)
        if fac and fac not in factions:
            factions.append(fac)

    return {
        "base_title": base,
        "qualifiers": qualifiers,
        "levels": levels,
        "factions": factions,
        "part_info": part_info,
    }


def make_title_lookup_keys(name: str) -> list[str]:
    """Generate deterministic lookup keys for matching wiki names against SDE missions/dungeons."""
    t = norm_title(name)
    if not t:
        return []
    variants = [t]
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
    if " - " in t:
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


def classify_trigger(text: str) -> dict[str, Any] | None:
    """Classify a free-form wiki trigger string into one of 7 canonical trigger kinds."""
    cleaned = clean_wiki_inline(text)
    if not cleaned:
        return None
    low = cleaned.lower()

    condition = "on_destroy"
    if re.search(r"\battack(?:ed|ing|ign|s)?\b|\baggress(?:ed|ing)?\b|\bshoot(?:ing)?\b", low):
        condition = "on_attack"
    elif re.search(r"\b(?:armor|shield|hull)\b|\b\d+\s*%\s*(?:hp|armor|hull|shield)\b|\bdamag?e", low):
        condition = "on_damage"

    unlocks_gate = bool(re.search(r"\bunlock(?:s|ing)?\s+(?:the\s+)?(?:acceleration\s+)?gate\b|\bgate\s+unlock\b", low))
    is_escalation = bool(re.search(r"\bescall?ation\b|\bexpedition\b", low))

    m_sec = re.search(r"\b(\d+)\s*(?:s|sec|seconds?)\b", low)
    m_min = re.search(r"\b(\d+)\s*(?:m|min|mins|minutes?)\b", low)
    m_hr = re.search(r"\b(\d+)\s*(?:h|hr|hrs|hours?)\b", low)
    time_seconds: int | None = None
    if m_sec:
        time_seconds = int(m_sec.group(1))
    elif m_min:
        time_seconds = int(m_min.group(1)) * 60
    elif m_hr:
        time_seconds = int(m_hr.group(1)) * 3600

    # Priority order from most specific to most general, ending at group_destroyed fallback
    if (
        re.search(
            r"\b(?:last|final)\b.*\b(?:of|ship\s+of|in)\b.*\b"
            r"(?:class|frigate|destroyer|cruiser|battlecruiser|battleship|industrial|drone)\b",
            low,
        )
        or re.search(r"\blast\s+(?:of|kill\s+of|ship\s+(?:killed\s+)?of)\s+(?:this|the|that)?\s*class\b", low)
        or re.search(r"\bdestroying\s+all\s+(?:the\s+)?(?:frigates|destroyers|cruisers|battlecruisers|battleships)\b", low)
        or re.search(r"\blast\s+lord\s+admiral\b", low)
    ):
        kind = "last_of_class_destroyed"
    elif (
        re.search(r"\b(?:last|final)\b.*\b(?:in|of)\b.*\b(?:group|wave|spawn|fleet|wing|pack)\b", low)
        or re.search(
            r"\blast\b(?:\s+one|\s+ship|\s+npc)?\s*(?:killed|destroyed|dead|left|triggers?|\bto\s+(?:die|spawn))\b",
            low,
        )
        or re.search(r"^last\b", low)
        or re.search(r"\ball\b.*\b(?:destroyed|killed|dead)\b", low)
    ):
        kind = "last_in_group_destroyed"
    elif re.search(
        r"\b(?:approach|approaching|within\s+\d+\s*km|proximity|warping\s+in|on\s+warp|range|distance|too\s+close|orbit|get\s+within|enter(?:s|ing)?)\b",
        low,
    ):
        kind = "ship_approached"
    elif time_seconds is not None or re.search(r"\b(?:timer|timed|delay|after\s+\d+|every\s+\d+|over\s+time)\b", low):
        kind = "timed"
    elif (
        unlocks_gate
        or re.search(
            r"\b(?:mission\s+objective|mission\s+complete|mission\s+completion|mission\s+targets?|mission\s+objects?|"
            r"complete\s+(?:the\s+)?mission|completes?\s+mission|\bobjective\b|ends?\s+(?:the\s+)?(?:expedition|mission|site)|"
            r"expedition\s+end|kill\s+to\s+complete|destroy\s+to\s+complete|needs?\s+to\s+be\s+killed|"
            r"hack(?:ed|ing)?|loot(?:ed|ing)?|drop(?:s|ped)?\b.*\bobjective)\b",
            low,
        )
    ):
        kind = "objective_complete"
    elif re.search(
        r"\b(?:attack(?:ed|ing|ign)?|shoot(?:ing)?|aggress|damag?ed?|armor|hull|shield|%\s*(?:hp|hull|armor|shield)|"
        r"escall?ations?|triggers?\s+(?:a\s+|the\s+|next\s+|additional\s+|more\s+|fighter\s+|delayed\s+)?(?:waves?|reinforcements?|spawns?|group\s*\d+|chain|neuting)|"
        r"spawns?\s+(?:next|waves?|reinforcements?|defenders?|group|two|more)|"
        r"killing\s+spawns?|\b(?:destroy|kill)\b.*\bto\s+spawn\b|destroying|destruction|upon\s+death|when\s+(?:killed|destroyed)|on\s+(?:death|destruction)|"
        r"highest\s+bounty|^yes$|^trigger$|^waves?\b|^next\s+wave|^reinforcements?|chain\s+trigger)\b",
        low,
    ):
        kind = "this_ship_destroyed"
    else:
        kind = "group_destroyed"

    assert kind in TRIGGER_ENUM
    res: dict[str, Any] = {
        "kind": kind,
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


def classify_wave_header_trigger(header: str) -> dict[str, Any] | None:
    """Extract trigger metadata when NPCTableHead/Separator encodes when a wave spawns."""
    cleaned = clean_wiki_inline(header)
    if not cleaned:
        return None
    low = cleaned.lower()
    # Look specifically for explicit condition phrases on wave headers (ignore plain "Wave 2", "Initial defenders", "Structures")
    m_paren = re.search(r"\(([^()]+)\)", cleaned)
    probe = None
    if m_paren and re.search(r"\b(?:destroy|kill|last|approach|min|sec|timer|when|on\s+|after|attack|hack|trigger)\b", m_paren.group(1), re.I):
        probe = m_paren.group(1)
    elif re.search(r"\b(?:triggered\s+by|on\s+destroying|after\s+\d+|within\s+\d+\s*km|last\s+(?:ship|in|of))\b", low):
        probe = cleaned
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


def extract_courier_payload(objective_raw: str, sde: "SdeIndex") -> dict[str, Any] | None:
    """Extract courier cargo item name, count, volume m3, and SDE item type IDs."""
    s = clean_wiki_inline(objective_raw)
    if not s:
        return None
    vol: float | None = None
    m_vol = re.search(r"\(\s*([\d,.]+)\s*m\s*(?:\^?3)?\s*\)|([\d,.]+)\s*m\s*(?:\^?3)\b", s, re.I)
    if m_vol:
        vol = _parse_number_token(m_vol.group(1) or m_vol.group(2))
    s_no_vol = re.sub(r"\(\s*[\d,.]+\s*m\s*(?:\^?3)?\s*\)|[\d,.]+\s*m\s*(?:\^?3)\b", " ", s, flags=re.I)
    s_no_vol = re.sub(r"\s+", " ", s_no_vol).strip()

    qty: int | None = None
    item: str | None = None
    m_q = re.search(
        r"(?:deliver|transport|haul|move|pick\s*up(?:\s+and\s+deliver)?|bring(?:\s+back)?|fetch|carry|ac?quire(?:\s+these\s+goods)?)\s*:?\s*"
        r"([\d,]+)\s*(?:x\s+)?(.+?)(?:\s+to\s+.+|\s+from\s+.+|$|[.;])",
        s_no_vol,
        re.I,
    )
    if m_q:
        qty = int(m_q.group(1).replace(",", ""))
        item = m_q.group(2).strip(" .,:;\"'")
    else:
        m_q2 = re.search(r"\b([\d,]+)\s*x\s+([A-Za-z][^.;()]+)", s_no_vol, re.I)
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
    vol: float | None = None
    m_vol = re.search(r"\(\s*([\d,.]+)\s*m\s*(?:\^?3)?\s*\)|([\d,.]+)\s*m\s*(?:\^?3)\b", s, re.I)
    if m_vol:
        vol = _parse_number_token(m_vol.group(1) or m_vol.group(2))
    s_no_vol = re.sub(r"\(\s*[\d,.]+\s*m\s*(?:\^?3)?\s*\)|[\d,.]+\s*m\s*(?:\^?3)\b", " ", s, flags=re.I)
    s_no_vol = re.sub(r"\s+", " ", s_no_vol).strip()

    qty: int | None = None
    ore: str | None = None
    patterns = (
        r"(?:mine|harvest|ac?quire(?:\s+these\s+goods)?|reprocess(?:\s+it)?\s+(?:to|into)|bring\s+back)\s*:?\s*"
        r"([\d,]+)\s*(?:x\s+)?(?:units\s+)?(?:of\s+)?([A-Za-z][A-Za-z0-9' \-]+?)"
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
    ore_type_ids = sde.lookup_item_type_ids(ore) if ore else []
    return {
        "ore_or_material": ore,
        "quantity": qty,
        "volume_m3": vol,
        "ore_type_ids": ore_type_ids,
        "raw": s,
    }


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
                break

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
    if "battlecruiser" in low:
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
        if not sde_db_path.exists():
            raise FileNotFoundError(f"SDE SQLite database not found: {sde_db_path}")
        conn = sqlite3.connect(str(sde_db_path))
        conn.row_factory = sqlite3.Row

        groups = {
            r["group_id"]: r["name"]
            for r in conn.execute("SELECT group_id, name FROM sde_groups")
        }

        self.npc_by_name: dict[str, list[SdeTypeRef]] = defaultdict(list)
        self.npc_by_nospace: dict[str, list[SdeTypeRef]] = defaultdict(list)
        self.npc_by_first_word_len: dict[tuple[str, int], list[str]] = defaultdict(list)
        self.ship_hulls: dict[str, list[SdeTypeRef]] = defaultdict(list)
        self.item_by_name: dict[str, list[int]] = defaultdict(list)
        self.faction_class_groups: dict[tuple[str, str], list[int]] = defaultdict(list)

        cat_list = ",".join(str(c) for c in ALLOWED_SDE_CATEGORIES)
        seen_first_words: set[tuple[str, int, str]] = set()
        for row in conn.execute(
            f"SELECT type_id, group_id, category_id, name, faction_id, volume "
            f"FROM sde_types WHERE name IS NOT NULL ORDER BY type_id"
        ):
            nl = row["name"].lower()
            self.item_by_name[nl].append(int(row["type_id"]))
            if row["category_id"] not in ALLOWED_SDE_CATEGORIES:
                continue
            gname = groups.get(row["group_id"], "")
            ref = SdeTypeRef(
                type_id=int(row["type_id"]),
                name=row["name"],
                group_id=int(row["group_id"]),
                category_id=int(row["category_id"]),
                faction_id=int(row["faction_id"]) if row["faction_id"] is not None else None,
                group_name=gname,
            )
            self.npc_by_name[nl].append(ref)
            self.npc_by_nospace[nl.replace(" ", "").replace("'", "")].append(ref)
            toks = nl.split()
            if toks:
                key = (toks[0], len(toks), nl)
                if key not in seen_first_words:
                    seen_first_words.add(key)
                    self.npc_by_first_word_len[(toks[0], len(toks))].append(nl)
            if ref.category_id == 6:
                self.ship_hulls[nl].append(ref)
            if ref.category_id == 11 and (
                gname.startswith("Mission ")
                or gname.startswith("Deadspace ")
                or gname.startswith("Asteroid ")
            ):
                fac = infer_faction_from_text(gname)
                if fac:
                    gn_norm = gname.lower().replace("battle cruisers", "battlecruiser")
                    for cls in ("frigate", "destroyer", "cruiser", "battlecruiser", "battleship"):
                        if cls in gn_norm:
                            self.faction_class_groups[(fac, cls)].append(ref.type_id)

        for k in self.faction_class_groups:
            self.faction_class_groups[k] = sorted(set(self.faction_class_groups[k]))[:20]
        for k in self.npc_by_first_word_len:
            self.npc_by_first_word_len[k].sort()

        self.dungeons_by_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in conn.execute(
            "SELECT dungeon_id, name, archetype_id, faction_id, difficulty FROM sde_dungeons ORDER BY dungeon_id"
        ):
            d = dict(row)
            if d["name"]:
                for k in make_title_lookup_keys(d["name"]):
                    self.dungeons_by_key[k].append(d)

        self.missions_by_key: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in conn.execute(
            "SELECT mission_id, name, is_courier, lp_alpha, lp_omega, has_standing_rewards "
            "FROM sde_missions ORDER BY mission_id"
        ):
            m = dict(row)
            if m["name"]:
                for k in make_title_lookup_keys(m["name"]):
                    self.missions_by_key[k].append(m)

        conn.close()

        self.mission_dungeons: dict[int, int] = {}
        if mission_ext_path.exists():
            ext_data = json.loads(mission_ext_path.read_text(encoding="utf-8"))
            for mid_str, did in ext_data.get("mission_dungeons", {}).items():
                self.mission_dungeons[int(mid_str)] = int(did)

    def lookup_item_type_ids(self, item_name: str | None) -> list[int]:
        if not item_name:
            return []
        clean = item_name.strip().lower()
        cands = [clean]
        if clean.endswith("s") and len(clean) > 3:
            cands.append(clean[:-1])
        else:
            cands.append(clean + "s")
        for prefix in ("crates of ", "crate of ", "large crate of ", "small crate of ", "group of ", "units of "):
            if clean.startswith(prefix):
                sub = clean[len(prefix) :].strip()
                cands.append(sub)
                cands.append(f"crate of {sub}")
        for c in cands:
            if c in self.item_by_name:
                return sorted(set(self.item_by_name[c]))
        return []

    @staticmethod
    def normalize_variants(name: str) -> list[str]:
        cands = [name]
        low = name.lower()
        for a, b in SPELLING_PAIRS:
            if a in low:
                cands.append(re.sub(re.escape(a), b, name, flags=re.I).strip())

        compound = name
        for a, b in (
            ("defence", "defense"),
            ("batery", "battery"),
            ("neutalizer", "neutralizer"),
            ("pithii", "pithi"),
            ("centi ", "centii "),
            ("war lord", "warlord"),
            ("sentinal", "sentinel"),
            ("blood raider", "bloodraider"),
            ("watchmen", "watchman"),
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
        if stripped != name:
            cands.append(stripped)
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
                cands.append(stripped[len(pf) :].strip())

        if low.startswith("corpum dark ") or low.startswith("corpum shadow "):
            cands.append("Dark " + stripped)
        if low.startswith("elder corpum dark ") or low.startswith("elder corpum shadow "):
            cands.append("Dark " + stripped[6:])
        if "-" in name:
            cands.append(name.replace("-", " "))
            cands.append(name.replace(" - ", "-"))

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
        else:
            cands.append(f"{stripped} I")

        out: list[str] = []
        seen: set[str] = set()
        for c in cands:
            cl = c.strip().lower()
            if cl and cl not in seen:
                seen.add(cl)
                out.append(c.strip())
        return out

    def direct_hits(self, name: str, allow_fuzzy: bool = True) -> list[SdeTypeRef]:
        for v in self.normalize_variants(name):
            vl = v.lower()
            if vl in self.npc_by_name:
                return self.npc_by_name[vl]
            ns = vl.replace(" ", "").replace("'", "")
            if ns in self.npc_by_nospace:
                return self.npc_by_nospace[ns]

        if not allow_fuzzy:
            return []

        nl = name.strip().lower()
        toks = nl.split()
        # Unambiguous trailing token abbreviation (e.g. "Corelum Guardian Chief Infa/Saf/Sen/Def/Prot")
        if len(toks) >= 2 and len(toks[-1]) >= 3:
            lead = " ".join(toks[:-1]) + " "
            last_t = toks[-1]
            abbr = [
                v
                for k, v in self.npc_by_name.items()
                if k.startswith(lead) and k[len(lead) :].startswith(last_t)
            ]
            if abbr:
                flat = [x for m in abbr for x in m]
                if len({x.name for x in flat}) <= 3:
                    return flat

        # Sibling rank substitution in the same pirate/empire/drone family
        if toks and toks[0] in FAMILY_RANK_SIBLINGS and len(toks) >= 2:
            rest = " ".join(toks[1:])
            for alt in FAMILY_RANK_SIBLINGS[toks[0]]:
                cand = f"{alt} {rest}"
                if cand in self.npc_by_name:
                    return self.npc_by_name[cand]
            if toks[-1] in ("alvi", "alvu", "alvatis", "alvior", "alvus"):
                body_p = " ".join(toks[:-1])
                for suf in ("alvi", "alvior", "alvu", "alvus", "alvatis"):
                    cand = f"{body_p} {suf}"
                    if cand in self.npc_by_name:
                        return self.npc_by_name[cand]

        # Guarded 1-typo difflib match sharing exact first token & token length
        if len(toks) >= 2:
            for base in (
                nl,
                re.sub(r"amarr navy ", "imperial navy ", re.sub(r"pithii", "pithi", nl)),
            ):
                btoks = base.split()
                pool = self.npc_by_first_word_len.get((btoks[0], len(btoks)), [])
                if pool:
                    matches = difflib.get_close_matches(base, pool, n=1, cutoff=0.85)
                    if matches:
                        return self.npc_by_name[matches[0]]
        return []

    def expand_short_names(self, raw_name: str) -> tuple[list[str], bool]:
        """Expand short-form EVE NPC names (slashes, shared prefix/suffix/circumfix, ranges, ellipses)."""
        s = clean_wiki_inline(raw_name)
        if not s or set(s) <= set("?-.* "):
            return [], False
        had_ellipsis = "..." in s

        # Numeric range brackets e.g. "AEGIS No. [1-4] Capital Construction Forge"
        m_rng = re.match(r"^(.*?)\[(\d+)\s*-\s*(\d+)\](.*)$", s)
        if m_rng and not self.direct_hits(s, allow_fuzzy=False):
            a, b = int(m_rng.group(2)), int(m_rng.group(3))
            if 0 < b - a <= 12:
                expanded = [
                    re.sub(r"\s+", " ", f"{m_rng.group(1)}{k}{m_rng.group(4)}").strip()
                    for k in range(a, b + 1)
                ]
                if any(self.direct_hits(x) for x in expanded):
                    return expanded, had_ellipsis

        # Embedded bracket/paren override e.g. "Seven Recruit [Coreli Watchman/Patroller]" or "Chemical Factory (Fortified Angel Bunker)"
        m_emb = re.match(r"^[^\[\(]+([\[\(])([^\]\)]+)([\]\)])\s*$", s)
        if m_emb and not self.direct_hits(s, allow_fuzzy=False):
            inner = m_emb.group(2).strip()
            if not re.search(
                r"\b(?:warning|km|units|m3|triggers|wave|boss|commander|spawn|note|optional|rare|maybe)\b",
                inner,
                re.I,
            ):
                exp_inner, _ = self.expand_short_names(inner)
                if exp_inner and any(self.direct_hits(x) for x in exp_inner):
                    return exp_inner, had_ellipsis

        def expand_piece(piece: str) -> list[str]:
            piece = piece.strip()
            if not piece:
                return []
            m_cnt = re.match(r"^(?:\d+x\s+|\d+\s*-\s*\d+\s*x?\s+)(.+)$", piece, re.I)
            if m_cnt and not self.direct_hits(piece, allow_fuzzy=False):
                piece = m_cnt.group(1).strip()
            if self.direct_hits(piece, allow_fuzzy=False):
                return [piece]

            # Parenthetical alternation: "Pithatis (Enforcer/Executor)"
            m = re.match(r"^(.*?)\(([^()]*?/[^()]*?)\)(.*)$", piece)
            if m:
                pre, inside, post = m.group(1), m.group(2), m.group(3)
                out: list[str] = []
                for opt in inside.split("/"):
                    opt = opt.strip()
                    if opt and opt != "...":
                        out.extend(expand_piece(re.sub(r"\s+", " ", f"{pre}{opt}{post}").strip()))
                return out

            if "/" in piece:
                parts = [p.strip() for p in piece.split("/") if p.strip()]
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
                    if sum(1 for c in hyp_cands if self.direct_hits(c, allow_fuzzy=False)) >= 2:
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

                parts = [re.sub(r"^\.\.\.\s*|\s*\.\.\.$", "", p).strip() for p in parts]
                parts = [p for p in parts if p]
                if not parts:
                    return []
                if len(parts) == 1:
                    return [parts[0]]
                if all(self.direct_hits(p) for p in parts):
                    return parts

                p0_w = parts[0].split()
                pl_w = parts[-1].split()

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
                                if idx > 0 and not cand.lower().startswith(prefix.lower()):
                                    cand = f"{prefix} {cand}"
                                if idx < len(parts) - 1 and not cand.lower().endswith(suffix.lower()):
                                    cand = f"{cand} {suffix}"
                                cands.append(cand)
                            hits = sum(1 for c in cands if self.direct_hits(c))
                            if hits > best_hits:
                                best_hits = hits
                                best = cands
                    if best and best_hits == len(best):
                        return best

                # Shared prefix ("Pithatis Enforcer/Executor", "Centus Tyrant/Dread Lord")
                best_pref: list[str] | None = None
                best_pref_hits = 0
                for kp in range(len(p0_w) - 1, 0, -1):
                    prefix = " ".join(p0_w[:kp])
                    cands = [parts[0]]
                    for p in parts[1:]:
                        if p.lower().startswith(prefix.lower()) or self.direct_hits(p, allow_fuzzy=False):
                            cands.append(p)
                        else:
                            cands.append(f"{prefix} {p}")
                    hits = sum(1 for c in cands if self.direct_hits(c))
                    if hits > best_pref_hits:
                        best_pref_hits = hits
                        best_pref = cands
                if best_pref and best_pref_hits == len(best_pref):
                    return best_pref

                # Shared suffix ("Heavy/Light Missile Battery", "Render/Decimator Alvi")
                best_suf: list[str] | None = None
                best_suf_hits = 0
                for ks in range(len(pl_w) - 1, 0, -1):
                    suffix = " ".join(pl_w[-ks:])
                    cands = []
                    for p in parts[:-1]:
                        if p.lower().endswith(suffix.lower()) or self.direct_hits(p, allow_fuzzy=False):
                            cands.append(p)
                        else:
                            cands.append(f"{p} {suffix}")
                    cands.append(parts[-1])
                    hits = sum(1 for c in cands if self.direct_hits(c))
                    if hits > best_suf_hits:
                        best_suf_hits = hits
                        best_suf = cands
                if best_suf and best_suf_hits > best_pref_hits:
                    return best_suf
                if best_pref:
                    return best_pref
                return parts

            return [piece]

        if "," in s and not self.direct_hits(s, allow_fuzzy=False):
            comma_parts = [x.strip() for x in s.split(",") if x.strip()]
            cand_list: list[str] = []
            for cp in comma_parts:
                cand_list.extend(expand_piece(cp))
            if len(cand_list) > 1 and any(self.direct_hits(x) for x in cand_list):
                return cand_list, had_ellipsis
        return expand_piece(s), had_ellipsis

    def find_family_candidates(self, name: str) -> list[SdeTypeRef]:
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
            for k in sorted(self.npc_by_name):
                if k.startswith(pref):
                    hits.extend(self.npc_by_name[k])
            if hits:
                return hits[:20]
        toks = low.split()
        if len(toks) >= 2 and toks[-1] in self.ship_hulls:
            return self.ship_hulls[toks[-1]]
        if len(toks) == 1 and len(low) >= 4:
            poss_prefix = low + "'s "
            hits = []
            for k in sorted(self.npc_by_name):
                if k.startswith(poss_prefix):
                    hits.extend(self.npc_by_name[k])
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
    ) -> dict[str, Any]:
        raw_clean = clean_wiki_inline(raw_name)
        cls_low = npc_class.lower()

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

        # Acceleration Gate rows
        if cls_low in ("acceleration gate", "accl gate", "accel gate") or re.match(
            r"^(?:acceleration )?gate\b", raw_clean.lower()
        ):
            hits = self.direct_hits("Acceleration Gate", allow_fuzzy=False)
            if hits:
                return {
                    "status": "resolved_exact",
                    "method": "acceleration_gate",
                    "expanded_names": [raw_clean],
                    "type_ids": sorted({h.type_id for h in hits}),
                    "candidate_type_ids": sorted({h.type_id for h in hits}),
                    "matched_names": sorted({h.name for h in hits}),
                    "unresolved_parts": [],
                    "reason": None,
                }

        # Asteroid / ore rows
        if cls_low == "asteroid":
            m_ast = ORE_STRIP_RE.match(raw_clean)
            if m_ast and m_ast.group(1).lower() in self.npc_by_name:
                hits = self.npc_by_name[m_ast.group(1).lower()]
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

        names, had_ellipsis = self.expand_short_names(raw_name)
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
            hits = self.direct_hits(nm, allow_fuzzy=True)
            if hits:
                exact_hits.extend(hits)
                continue

            if note:
                m_ren = re.search(
                    r"(?:renamed(?:\s+to|\s+from)?|formerly|previously|now)\s+['\"]?([A-Za-z0-9' \-]+)",
                    note,
                    re.I,
                )
                if m_ren:
                    hits = self.direct_hits(m_ren.group(1).strip(), allow_fuzzy=True)
                    if hits:
                        exact_hits.extend(hits)
                        continue

            nlow = re.sub(r"\s*[\(\[][^)\]]*[\)\]]", "", nm).strip(" .?*!").lower()
            nlow_nopf = re.sub(r"^(?:starbase|pirate|enemy|hostile)\s+", "", nlow)
            cand_patterns: list[str] = []
            for key in (nlow, nlow_nopf):
                if key in STRUCTURE_ALIASES:
                    cand_patterns.extend(STRUCTURE_ALIASES[key])
            if cls_low in (
                "structure",
                "sentry",
                "tower",
                "container",
                "hackable",
                "destructible object",
                "large collidable object",
                "battery",
                "drone",
                "frigate",
                "cruiser",
                "battleship",
                "battlecruiser",
                "destroyer",
                "industrial",
                "hangar array",
                "beacon",
            ):
                cand_patterns.append("{fac} " + nm)

            struct_matched: list[SdeTypeRef] = []
            fac_pool = page_factions or [
                "Angel Cartel",
                "Guristas Pirates",
                "Blood Raider Covenant",
                "Sansha's Nation",
                "Serpentis",
                "Rogue Drones",
            ]
            for fac in fac_pool:
                prefixes = FACTION_PREFIXES.get(fac, [fac])
                for pat in cand_patterns:
                    if "{fac}" in pat:
                        for pf in prefixes:
                            h = self.direct_hits(pat.format(fac=pf), allow_fuzzy=False)
                            if h:
                                struct_matched.extend(h)
                    else:
                        h = self.direct_hits(pat, allow_fuzzy=False)
                        if h:
                            struct_matched.extend(h)
                    if struct_matched:
                        break
                if struct_matched:
                    break
            if struct_matched:
                struct_hits.extend(struct_matched)
                continue

            fh = self.find_family_candidates(nm)
            if fh:
                fam_hits.extend(fh)
            else:
                unresolved_parts.append(nm)

        all_refs = exact_hits + struct_hits + fam_hits
        all_tids = sorted({h.type_id for h in all_refs})
        matched_names = sorted({h.name for h in all_refs})

        if not unresolved_parts:
            if fam_hits:
                status = "resolved_family_candidates"
                method = "ellipsis_or_family_prefix" if had_ellipsis else "family_or_hull_candidate"
            elif struct_hits:
                status = "resolved_structure_faction"
                method = "faction_qualified_structure"
            else:
                status = "resolved_exact"
                method = "short_form_expansion" if len(names) > 1 else "exact_or_normalized"
            return {
                "status": status,
                "method": method,
                "expanded_names": names,
                "type_ids": sorted({h.type_id for h in exact_hits + struct_hits}),
                "candidate_type_ids": all_tids,
                "matched_names": matched_names,
                "unresolved_parts": [],
                "reason": None,
            }

        if all_refs:
            return {
                "status": "partly_resolved",
                "method": "partial_multi_name",
                "expanded_names": names,
                "type_ids": sorted({h.type_id for h in exact_hits + struct_hits}),
                "candidate_type_ids": all_tids,
                "matched_names": matched_names,
                "unresolved_parts": unresolved_parts,
                "reason": f"unresolved_part:{';'.join(unresolved_parts)}",
            }

        # Fallback to faction + ship_class mission/site SDE entity group candidates ONLY when
        # the custom spawn name indicates a faction or EVE combat role/class
        cb = _norm_class_bucket(npc_class)
        inferred_name_fac = infer_faction_from_text(raw_clean)
        nf = inferred_name_fac or (page_factions[0] if page_factions else None)
        has_role_or_fac_hint = bool(inferred_name_fac or NPC_ROLE_HINT_RE.search(raw_clean))
        if has_role_or_fac_hint and cb and nf and self.faction_class_groups.get((nf, cb)):
            cands = self.faction_class_groups[(nf, cb)]
            return {
                "status": "resolved_class_candidates",
                "method": f"faction_class_group:{nf}:{cb}",
                "expanded_names": names,
                "type_ids": [],
                "candidate_type_ids": cands,
                "matched_names": [],
                "unresolved_parts": unresolved_parts,
                "reason": "custom_spawn_name_mapped_via_faction_class",
            }

        return {
            "status": "unresolved",
            "method": "none",
            "expanded_names": names,
            "type_ids": [],
            "candidate_type_ids": [],
            "matched_names": [],
            "unresolved_parts": unresolved_parts,
            "reason": "event_npc_not_in_sde" if is_event else "unmatched_name_in_sde",
        }

    def match_sde_page(
        self,
        candidate_names: list[str],
        factions: list[str],
        levels: list[int],
    ) -> dict[str, Any]:
        """Match a wiki unit/page against sde_missions, mission_dungeons, and sde_dungeons."""
        fac_ids = {CANON_FACTION_NAME_TO_ID[f] for f in factions if f in CANON_FACTION_NAME_TO_ID}
        matched_missions: dict[int, dict[str, Any]] = {}
        matched_dungeons: dict[int, dict[str, Any]] = {}

        for nm in candidate_names:
            for key in make_title_lookup_keys(nm):
                for m in self.missions_by_key.get(key, []):
                    matched_missions[m["mission_id"]] = m
                    if m["mission_id"] in self.mission_dungeons:
                        did = self.mission_dungeons[m["mission_id"]]
                        # Ensure linked dungeon ID is recorded even if dungeon name differed
                        matched_dungeons.setdefault(
                            did,
                            {"dungeon_id": did, "name": m["name"], "faction_id": None, "difficulty": None, "linked_via_mission_ext": True},
                        )
                for d in self.dungeons_by_key.get(key, []):
                    matched_dungeons[d["dungeon_id"]] = d

        # Narrow dungeon candidates when multiple factions/difficulties exist and page specifies them
        dungeon_list = list(matched_dungeons.values())
        if len(dungeon_list) > 1 and fac_ids:
            fac_filtered = [d for d in dungeon_list if d.get("faction_id") in fac_ids]
            if fac_filtered:
                dungeon_list = fac_filtered
        if len(dungeon_list) > 1 and levels:
            lvl_filtered = [d for d in dungeon_list if d.get("difficulty") in levels]
            if lvl_filtered:
                dungeon_list = lvl_filtered

        mission_ids = sorted(matched_missions)
        dungeon_ids = sorted({d["dungeon_id"] for d in dungeon_list})
        return {
            "mission_ids": mission_ids,
            "dungeon_ids": dungeon_ids,
            "missions": [matched_missions[mid] for mid in mission_ids],
            "dungeons": sorted(dungeon_list, key=lambda d: d["dungeon_id"]),
        }


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
    has_pve_cat = bool(set(categories) & PVE_CATEGORIES)
    if not has_pve_template and not (has_pve_cat and "NPCTableRow" in wikitext):
        return None
    if not has_pve_template:
        return None

    tinfo = parse_title_qualifiers(title)
    is_event = "Game events" in categories
    is_past = "Past content" in categories

    # Build raw units grouped around Missiondetails / CMBSite* headers (skipping Escalation teaser links)
    units_raw: list[dict[str, Any]] = []
    cur_heading_stack: list[tuple[int, str]] = []
    cur_unit: dict[str, Any] | None = None
    preamble_events: list[dict[str, Any]] = []

    def current_heading_text() -> str:
        return " > ".join(h for _, h in cur_heading_stack)

    for idx, ev in enumerate(events):
        if ev["kind"] == "heading":
            lvl = ev["level"]
            txt = ev["text"]
            cur_heading_stack = [(l, h) for l, h in cur_heading_stack if l < lvl]
            cur_heading_stack.append((lvl, txt))
            if cur_unit is not None:
                cur_unit["events"].append(ev)
            else:
                preamble_events.append(ev)
            continue

        tname = ev["name"]
        if tname == "missiondetails":
            cur_unit = {
                "unit_kind": "mission",
                "header": ev,
                "section_heading": current_heading_text(),
                "events": list(preamble_events) if not units_raw and preamble_events else [],
                "escalation_links": [],
            }
            preamble_events.clear()
            units_raw.append(cur_unit)
        elif tname in ("cmbsiteinfo", "cmbsite", "cmbsitebox"):
            # Check if this is an Escalation teaser with no NPC rows following it when we already have a unit
            if tname in ("cmbsite", "cmbsitebox") and units_raw:
                in_esc_section = "escalation" in current_heading_text().lower()
                # Look ahead for any NPC rows before next unit header
                has_following_rows = False
                for nxt in events[idx + 1 :]:
                    if nxt["kind"] == "template" and nxt["name"] in ("missiondetails", "cmbsiteinfo", "cmbsite", "cmbsitebox"):
                        break
                    if nxt["kind"] == "template" and nxt["name"] in ("npctablerow", "npctr"):
                        has_following_rows = True
                        break
                if in_esc_section and not has_following_rows:
                    link_nm = ev["kv"].get("name") or (ev["pos_args"][0] if ev["pos_args"] else "")
                    if link_nm:
                        units_raw[-1]["escalation_links"].append(clean_wiki_inline(link_nm))
                    continue
            cur_unit = {
                "unit_kind": "combat_site",
                "header": ev,
                "section_heading": current_heading_text(),
                "events": list(preamble_events) if not units_raw and preamble_events else [],
                "escalation_links": [],
            }
            preamble_events.clear()
            units_raw.append(cur_unit)
        else:
            if cur_unit is not None:
                cur_unit["events"].append(ev)
            else:
                preamble_events.append(ev)

    if not units_raw:
        # Page has NPCTableRow / NPCTableHead without Missiondetails / CMBSiteInfo
        unit_kind = "combat_site" if set(categories) & {
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
        } else "mission"
        units_raw.append(
            {
                "unit_kind": unit_kind,
                "header": None,
                "section_heading": "",
                "events": preamble_events,
                "escalation_links": [],
            }
        )

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
                fac = infer_faction_from_text(clean_wiki_inline(kv[fk]))
                if fac and fac not in factions:
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
                fac = infer_faction_from_text(clean_wiki_inline(kv[fk]))
                if fac and fac not in factions:
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
            f = infer_faction_from_text(probe)
            if f and f not in factions:
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
        nonlocal cur_room, cur_wave, pending_room_name
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

    def start_wave(wname: str) -> None:
        nonlocal cur_wave
        room = ensure_room()
        wtrig = classify_wave_header_trigger(wname)
        cur_wave = {
            "wave_index": len(room["waves"]) + 1,
            "wave_name": wname or f"Wave {len(room['waves']) + 1}",
            "wave_trigger": wtrig,
            "spawns": [],
        }
        room["waves"].append(cur_wave)

    for ev in uraw["events"]:
        if ev["kind"] == "heading":
            htxt = ev["text"]
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
            # Also detect if NPCTableHead itself names a pocket/room when heading didn't
            if wname and re.match(r"^(?:pocket|room)\s+\d+\b", wname, re.I) and cur_room is not None and cur_room["waves"]:
                start_room(wname)
            start_wave(wname)
        elif tname in ("npctableseparator", "npcts"):
            sep_label = clean_wiki_inline(epos[0] if epos else "")
            if sep_label and re.match(r"^(?:pocket|room)\s+\d+\b", sep_label, re.I):
                start_room(sep_label)
            else:
                start_wave(sep_label or "Group")
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

    # Drop completely empty rooms if a room heading had no spawns at all
    rooms = [r for r in rooms if any(w["spawns"] for w in r["waves"])]
    for idx, r in enumerate(rooms, start=1):
        r["room_index"] = idx

    # Extract courier & mining payloads from Missiondetails
    mtype_raw = clean_wiki_inline(kv.get("type", "")) if hdr else ""
    obj_raw = kv.get("objective", "") or kv.get("objestive", "") if hdr else ""
    courier_payload = None
    mining_payload = None
    if hdr and hdr["name"] == "missiondetails":
        mt_low = mtype_raw.lower()
        if "courier" in mt_low or "distribution" in mt_low or "trade" in mt_low:
            courier_payload = extract_courier_payload(obj_raw, sde)
        if "mining" in mt_low or re.search(r"\b(?:mine|harvest)\b", clean_wiki_inline(obj_raw).lower()):
            mining_payload = extract_mining_payload(obj_raw, sde)

    # Build SDE matching candidate names (including section heading level/part hints)
    sde_match_names = list(unit_names)
    if uraw["section_heading"]:
        sh = uraw["section_heading"]
        if re.search(r"\(\d+\s+of\s+\d+\)", sh):
            sde_match_names.append(sh)
    sde_match = sde.match_sde_page(sde_match_names, factions, levels)

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

    entry_id = f"{page_title}#{unit_index}"
    return {
        "entry_id": entry_id,
        "page_title": page_title,
        "base_title": tinfo["base_title"],
        "unit_index": unit_index,
        "unit_kind": uraw["unit_kind"],
        "section_heading": uraw["section_heading"],
        "title_qualifiers": tinfo["qualifiers"],
        "part_info": tinfo["part_info"],
        "categories": sorted(categories),
        "is_event": is_event,
        "is_past_content": is_past,
        "factions": factions,
        "levels": levels,
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


def run_import(
    wiki_dir: Path,
    sde_db_path: Path,
    mission_ext_path: Path,
    out_content_path: Path,
    out_report_path: Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Run full wiki PvE import pipeline and write content + report JSON deterministically."""
    sde = SdeIndex(sde_db_path, mission_ext_path)
    articles, catalog_summary = _collect_wiki_articles(wiki_dir)

    entries: list[dict[str, Any]] = []
    entries_by_mission_id: dict[str, list[str]] = defaultdict(list)
    entries_by_dungeon_id: dict[str, list[str]] = defaultdict(list)

    status_counts: Counter[str] = Counter()
    trigger_counts: Counter[str] = Counter()
    partly_resolved_rows: list[dict[str, Any]] = []
    unresolved_rows: list[dict[str, Any]] = []
    trigger_fallbacks: list[dict[str, Any]] = []
    page_reports: list[dict[str, Any]] = []

    pve_pages_count = 0
    courier_count = 0
    mining_count = 0
    matched_mission_units = 0
    matched_dungeon_units = 0

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
        pve_pages_count += 1

        p_rows_tot = 0
        p_rows_resolved = 0
        p_rows_partial = 0
        p_rows_unresolved = 0
        p_trig_fb = 0
        p_matched_sde = False

        for unit in parsed["units"]:
            entries.append(unit)
            eid = unit["entry_id"]
            if unit["courier"]:
                courier_count += 1
            if unit["mining"]:
                mining_count += 1
            mids = unit["sde_matches"]["mission_ids"]
            dids = unit["sde_matches"]["dungeon_ids"]
            if mids:
                matched_mission_units += 1
                p_matched_sde = True
                for mid in mids:
                    entries_by_mission_id[str(mid)].append(eid)
            if dids:
                matched_dungeon_units += 1
                p_matched_sde = True
                for did in dids:
                    entries_by_dungeon_id[str(did)].append(eid)

            for room in unit["rooms"]:
                for wave in room["waves"]:
                    wtrig = wave.get("wave_trigger")
                    if wtrig:
                        trigger_counts[wtrig["kind"]] += 1
                        if wtrig["kind"] == "group_destroyed":
                            p_trig_fb += 1
                            trigger_fallbacks.append(
                                {
                                    "page": art["title"],
                                    "entry_id": eid,
                                    "room": room["room_name"],
                                    "wave": wave["wave_name"],
                                    "npc_name": None,
                                    "original_text": wtrig["original_text"],
                                }
                            )
                    for sp in wave["spawns"]:
                        p_rows_tot += 1
                        res = sp["resolution"]
                        st = res["status"]
                        status_counts[st] += 1
                        if st.startswith("resolved_"):
                            p_rows_resolved += 1
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
                            p_rows_unresolved += 1
                            unresolved_rows.append(
                                {
                                    "page": art["title"],
                                    "entry_id": eid,
                                    "room": room["room_name"],
                                    "wave": wave["wave_name"],
                                    "raw_class": sp["raw_class"],
                                    "raw_name": sp["raw_name"],
                                    "factions": unit["factions"],
                                    "reason": res["reason"],
                                }
                            )

                        tr = sp.get("trigger")
                        if tr:
                            trigger_counts[tr["kind"]] += 1
                            if tr["kind"] == "group_destroyed":
                                p_trig_fb += 1
                                trigger_fallbacks.append(
                                    {
                                        "page": art["title"],
                                        "entry_id": eid,
                                        "room": room["room_name"],
                                        "wave": wave["wave_name"],
                                        "npc_name": sp["raw_name"],
                                        "original_text": tr["original_text"],
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
                "trigger_fallback_count": p_trig_fb,
            }
        )

    npc_total = sum(status_counts.values())
    npc_resolved_total = sum(v for k, v in status_counts.items() if k.startswith("resolved_"))
    npc_strict_resolved = (
        status_counts.get("resolved_exact", 0)
        + status_counts.get("resolved_structure_faction", 0)
        + status_counts.get("resolved_family_candidates", 0)
    )
    resolved_pct = round(100.0 * npc_resolved_total / npc_total, 2) if npc_total else 0.0
    strict_pct = round(100.0 * npc_strict_resolved / npc_total, 2) if npc_total else 0.0

    header = {
        "schema_version": 1,
        "source": "EVE University Wiki (~/ai/eve-wiki) + local client SDE",
        "license": "CC BY-SA 4.0 (locally generated from wiki copy; never commit to git)",
        "wiki_catalog_summary": catalog_summary,
        "deterministic": True,
    }

    content_doc: dict[str, Any] = {
        "header": header,
        "entries": entries,
        "indexes": {
            "by_mission_id": {k: sorted(v) for k, v in sorted(entries_by_mission_id.items())},
            "by_dungeon_id": {k: sorted(v) for k, v in sorted(entries_by_dungeon_id.items())},
        },
    }

    report_doc: dict[str, Any] = {
        "header": header,
        "totals": {
            "pages_scanned": len(articles),
            "pve_pages_extracted": pve_pages_count,
            "content_units": len(entries),
            "units_matched_to_sde_mission": matched_mission_units,
            "units_matched_to_sde_dungeon": matched_dungeon_units,
            "units_with_courier_payload": courier_count,
            "units_with_mining_payload": mining_count,
            "npc_rows_total": npc_total,
            "npc_rows_resolved_total": npc_resolved_total,
            "npc_rows_resolved_percent": resolved_pct,
            "npc_rows_strict_resolved_total": npc_strict_resolved,
            "npc_rows_strict_resolved_percent": strict_pct,
            "npc_rows_partly_resolved": status_counts.get("partly_resolved", 0),
            "npc_rows_unresolved": status_counts.get("unresolved", 0),
            "npc_resolution_breakdown": {k: status_counts[k] for k in sorted(status_counts)},
            "trigger_total": sum(trigger_counts.values()),
            "trigger_classification_counts": {k: trigger_counts[k] for k in sorted(trigger_counts)},
            "trigger_fallback_count": len(trigger_fallbacks),
        },
        "trigger_fallbacks": trigger_fallbacks,
        "partly_resolved_rows": partly_resolved_rows,
        "unresolved_rows": unresolved_rows,
        "pages": page_reports,
    }

    out_content_path.parent.mkdir(parents=True, exist_ok=True)
    out_report_path.parent.mkdir(parents=True, exist_ok=True)
    out_content_path.write_text(
        json.dumps(content_doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    out_report_path.write_text(
        json.dumps(report_doc, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return content_doc, report_doc


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
        "--verify",
        action="store_true",
        help="Run import twice and verify deterministic SHA-256 hashes, accounting invariants, and resolution threshold",
    )
    parser.add_argument(
        "--min-resolved-percent",
        type=float,
        default=90.0,
        help="Minimum NPC row resolved percentage enforced by --verify (default: 90.0)",
    )
    args = parser.parse_args(argv)

    _, report = run_import(
        wiki_dir=args.wiki_dir,
        sde_db_path=args.sde_db,
        mission_ext_path=args.mission_ext,
        out_content_path=args.out_content,
        out_report_path=args.out_report,
    )
    totals = report["totals"]
    print(
        f"Imported {totals['content_units']} PvE units across {totals['pve_pages_extracted']} pages "
        f"(missions_matched={totals['units_matched_to_sde_mission']}, "
        f"dungeons_matched={totals['units_matched_to_sde_dungeon']}); "
        f"NPC rows resolved: {totals['npc_rows_resolved_total']}/{totals['npc_rows_total']} "
        f"({totals['npc_rows_resolved_percent']}%); "
        f"partial={totals['npc_rows_partly_resolved']}, unresolved={totals['npc_rows_unresolved']}; "
        f"trigger_fallbacks={totals['trigger_fallback_count']}"
    )

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
            )
            sha_c1 = hashlib.sha256(args.out_content.read_bytes()).hexdigest()
            sha_c2 = hashlib.sha256(out_content_2.read_bytes()).hexdigest()
            sha_r1 = hashlib.sha256(args.out_report.read_bytes()).hexdigest()
            sha_r2 = hashlib.sha256(out_report_2.read_bytes()).hexdigest()

        errors: list[str] = []
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
                f"NPC resolved percent {totals['npc_rows_resolved_percent']}% < {args.min_resolved_percent}%"
            )
        if totals["npc_rows_partly_resolved"] != len(report["partly_resolved_rows"]):
            errors.append("partly_resolved_rows accounting mismatch")
        if totals["npc_rows_unresolved"] != len(report["unresolved_rows"]):
            errors.append("unresolved_rows accounting mismatch")
        if totals["trigger_fallback_count"] != len(report["trigger_fallbacks"]):
            errors.append("trigger_fallbacks accounting mismatch")
        if errors:
            for err in errors:
                print(f"VERIFY FAIL: {err}", file=sys.stderr)
            return 1
        print(
            f"VERIFY PASS: content_sha256={sha_c1} report_sha256={sha_r1} "
            f"resolved={totals['npc_rows_resolved_percent']}% (>= {args.min_resolved_percent}%)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
